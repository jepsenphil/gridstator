//! Snapshot globs use picomatch's POSIX, `dot: true` rules, rather than gitignore rules.
//! In particular, each array entry is an independent OR, leading `!` complements a matcher,
//! and negative extglobs are prefix assertions (with picomatch's terminal/suffix exceptions).
//!
//! This small native pattern VM uses UTF-16 positions, like the reference JavaScript regexes.
//! Memoized end positions and a visited set for repetition avoid recursive backtracking loops.
use anyhow::{Result, ensure};
use std::collections::{BTreeSet, HashMap};
mod regex;

#[derive(Clone, Default)]
pub struct SnapshotPatterns(Vec<Pattern>);

impl SnapshotPatterns {
    pub fn new(patterns: &[String]) -> Result<Self> {
        patterns
            .iter()
            .map(|pattern| Pattern::new(pattern))
            .collect::<Result<Vec<_>>>()
            .map(Self)
    }

    pub fn is_match(&self, path: &str) -> Result<bool> {
        let mut work = 0;
        if !path.is_empty() {
            for (index, pattern) in self.0.iter().enumerate() {
                if pattern.is_match_budgeted(path, &mut work, self.0.len() - index)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Match the reference's include walk roots. A literal file pattern has that file as its
    /// base; walking a non-directory adds no candidates, even if other array entries match them.
    pub fn bases(&self) -> BTreeSet<String> {
        self.0
            .iter()
            .map(|pattern| scan_base(&pattern.raw))
            .collect()
    }
}

#[derive(Clone)]
struct Pattern {
    raw: String,
    negated: bool,
    nodes: Vec<Node>,
    root: usize,
    needs_regex: bool,
    parentheses: usize,
    regex: Option<regex::Compiled>,
}

#[derive(Clone)]
enum Node {
    Never,
    Literal(Vec<u16>),
    Any,
    Star {
        component_start: bool,
    },
    Globstar,
    Directories,
    Tail,
    Class {
        ranges: Vec<(u16, u16)>,
        negated: bool,
    },
    Sequence(Vec<usize>),
    Alternative(Vec<usize>),
    Repeat {
        inner: usize,
        minimum: usize,
        once: bool,
    },
    Negative {
        inner: usize,
        anchored: bool,
        slash: bool,
    },
    Group {
        prefix: String,
        inner: usize,
    },
    Raw(String),
}

impl Pattern {
    fn new(raw: &str) -> Result<Self> {
        ensure!(!raw.is_empty(), "Expected pattern to be a non-empty string");
        ensure!(
            raw.encode_utf16().count() <= 65_536,
            "Snapshot pattern exceeds picomatch's maximum length of 65536"
        );
        check_depth(raw)?;
        let mut pattern = Self {
            raw: raw.into(),
            negated: false,
            nodes: Vec::new(),
            root: 0,
            needs_regex: false,
            parentheses: 0,
            regex: None,
        };
        let mut rest = raw.strip_prefix("./").unwrap_or(raw);
        while rest.starts_with('!') && !negative_extglob_start(rest) {
            pattern.negated = !pattern.negated;
            rest = &rest[1..];
        }
        rest = rest.strip_prefix("./").unwrap_or(rest);
        let chars: Vec<_> = rest.chars().collect();
        let full_parser =
            rest.starts_with(['*', '!']) || rest.contains(['/', '(', ')', '[', ']', '{', '}', '"']);
        pattern.root = if full_parser && split(&chars, '|').len() > 1 {
            pattern.needs_regex = true;
            pattern.alternatives(&chars, '|', "")
        } else {
            pattern.sequence(&chars, "")
        };
        if pattern.needs_regex {
            pattern.regex = Some(regex::Compiled::new(&pattern)?);
        }
        Ok(pattern)
    }

    fn node(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn literal(&mut self, text: &str) -> usize {
        self.node(Node::Literal(text.encode_utf16().collect()))
    }

    fn alternatives(&mut self, body: &[char], separator: char, continuation: &str) -> usize {
        let alternatives = split(body, separator)
            .into_iter()
            .map(|part| self.sequence(part, continuation))
            .collect();
        self.node(Node::Alternative(alternatives))
    }

    fn sequence(&mut self, text: &[char], continuation: &str) -> usize {
        let mut children = Vec::new();
        let mut i = 0;
        let mut previous = 't';
        while i < text.len() {
            let ch = text[i];
            let prior = previous;
            previous = 't';
            if ch == '\\' {
                i += 1;
                if let Some(ch) = text.get(i) {
                    let digits = if *ch == 'u' {
                        4
                    } else if *ch == 'x' {
                        2
                    } else {
                        0
                    };
                    if digits > 0
                        && text
                            .get(i + 1..i + 1 + digits)
                            .is_some_and(|digits| digits.iter().all(char::is_ascii_hexdigit))
                    {
                        let hex: String = text[i + 1..i + 1 + digits].iter().collect();
                        children.push(
                            self.node(Node::Literal(vec![u16::from_str_radix(&hex, 16).unwrap()])),
                        );
                        i += digits;
                    } else if *ch == 'k'
                        && text.get(i + 1) == Some(&'<')
                        && text[i + 2..].contains(&'>')
                    {
                        let close = i + 2 + text[i + 2..].iter().position(|ch| *ch == '>').unwrap();
                        self.needs_regex = true;
                        children.push(self.node(Node::Raw(format!(
                            "\\{}",
                            text[i..=close].iter().collect::<String>()
                        ))));
                        i = close;
                    } else if ch.is_ascii_alphanumeric() {
                        self.needs_regex = true;
                        children.push(self.node(Node::Raw(format!("\\{ch}"))));
                    } else {
                        children.push(self.literal(&ch.to_string()));
                    }
                }
                previous = 't';
            } else if ch == '"' {
                i += 1;
                let start = i;
                while i < text.len() && text[i] != '"' {
                    i += 1;
                }
                children.push(self.literal(&text[start..i].iter().collect::<String>()));
                previous = 't';
            } else if "@+?*!".contains(ch)
                && text.get(i + 1) == Some(&'(')
                && !(ch != '!' && text.get(i + 2) == Some(&'?'))
                && !(ch == '!'
                    && text.get(i + 2) == Some(&'?')
                    && text.get(i + 3).is_some_and(|ch| "!=<:".contains(*ch)))
            {
                if let Some(close) = closing(text, i + 1, '(', ')') {
                    let body = &text[i + 2..close];
                    let tail: String = text[close + 1..].iter().collect();
                    let following = format!("{tail}{continuation}");
                    let inner_continuation = format!("){following}");
                    self.parentheses += 1;
                    let mut inner = self.alternatives(body, '|', &inner_continuation);
                    self.parentheses -= 1;
                    let node = match ch {
                        '@' => self.node(Node::Group {
                            prefix: String::new(),
                            inner,
                        }),
                        '?' => self.node(Node::Repeat {
                            inner,
                            minimum: 0,
                            once: true,
                        }),
                        '*' | '+' => match analyze_repeat(body) {
                            RepeatAnalysis::Literal => {
                                self.literal(&text[i..=close].iter().collect::<String>())
                            }
                            RepeatAnalysis::Characters(chars) => {
                                let inner = self.node(Node::Class {
                                    ranges: merge_ranges(
                                        chars.into_iter().map(|ch| (ch, ch)).collect(),
                                    ),
                                    negated: false,
                                });
                                self.node(Node::Repeat {
                                    inner,
                                    minimum: 0,
                                    once: false,
                                })
                            }
                            RepeatAnalysis::Safe => self.node(Node::Repeat {
                                inner,
                                minimum: usize::from(ch == '+'),
                                once: false,
                            }),
                        },
                        '!' => {
                            let slash = body.contains(&'/') && body.len() > 1;
                            let mut anchored = slash
                                || following.is_empty()
                                || following.chars().all(|ch| ch == ')');
                            // Picomatch folds a dotted suffix into the assertion for wildcard
                            // bodies, e.g. !(*.test).ts and !(*.d).{ts,tsx}.
                            if body.contains(&'*')
                                && following.starts_with('.')
                                && !following[1..].is_empty()
                                && !following[1..].contains(['/', '\\', '.'])
                            {
                                let suffix =
                                    self.sequence(&following.chars().collect::<Vec<_>>(), "");
                                inner = self.node(Node::Sequence(vec![inner, suffix]));
                                anchored = false;
                            }
                            self.node(Node::Negative {
                                inner,
                                anchored,
                                slash,
                            })
                        }
                        _ => unreachable!(),
                    };
                    children.push(node);
                    i = close;
                    previous = 'p';
                } else {
                    // The reference discards an unclosed extglob operator and escapes '(' .
                    children.push(self.literal("("));
                    i += 1;
                }
            } else if ch == '(' {
                if let Some(close) = closing(text, i, '(', ')') {
                    let following = format!(
                        "){}{continuation}",
                        text[close + 1..].iter().collect::<String>()
                    );
                    self.needs_regex = true;
                    let body = &text[i + 1..close];
                    let (prefix, offset) = raw_group(body);
                    self.parentheses += 1;
                    let inner = self.alternatives(&body[offset..], '|', &following);
                    self.parentheses -= 1;
                    children.push(self.node(Node::Group { prefix, inner }));
                    i = close;
                    previous = 'p';
                } else {
                    children.push(self.literal("("));
                }
            } else if ch == '{' {
                if let Some(close) = closing(text, i, '{', '}') {
                    let body = &text[i + 1..close];
                    let parts = split(body, ',');
                    if parts.len() > 1 {
                        let following = format!(
                            "}}{}{continuation}",
                            text[close + 1..].iter().collect::<String>()
                        );
                        let inner = self.alternatives(body, ',', &following);
                        children.push(self.node(Node::Group {
                            prefix: String::new(),
                            inner,
                        }));
                    } else {
                        let body_text: String = body.iter().collect();
                        if body_text.contains("..") {
                            let mut bounds: Vec<_> = body_text.split("..").collect();
                            bounds.sort_unstable();
                            let class = format!("[{}]", bounds.join("-"));
                            children.push(self.class(&class.chars().collect::<Vec<_>>()));
                        } else {
                            children.push(self.literal("{"));
                            children.push(self.sequence(body, continuation));
                            children.push(self.literal("}"));
                        }
                    }
                    i = close;
                    previous = 'r';
                } else {
                    children.push(self.node(Node::Never));
                }
            } else if ch == '[' {
                if let Some(close) = bracket_end(text, i) {
                    children.push(self.class(&text[i..=close]));
                    i = close;
                    previous = 'b';
                } else {
                    children.push(self.literal("["));
                }
            } else if ch == '/' && text.get(i + 1..) == Some(&['*', '*']) {
                children.push(self.node(Node::Tail));
                i += 2;
            } else if ch == '*' {
                let start = i;
                while text.get(i + 1) == Some(&'*') {
                    i += 1;
                }
                let boundary = start == 0 || text[start - 1] == '/';
                if i - start == 1 && boundary && (i + 1 == text.len() || text[i + 1] == '/') {
                    if text.get(i + 1) == Some(&'/') {
                        children.push(self.node(Node::Directories));
                        i += 1;
                    } else {
                        children.push(self.node(Node::Globstar));
                    }
                } else {
                    children.push(self.node(Node::Star {
                        component_start: boundary,
                    }));
                }
            } else if (ch == '?' && prior == 'p')
                || (ch == '+'
                    && (matches!(prior, 'p' | 'b' | 'r') || (self.parentheses > 0 && i > 0)))
            {
                self.needs_regex = true;
                children.push(self.node(Node::Raw(ch.to_string())));
                previous = 't';
            } else if ch == '?' && i == 0 && self.parentheses > 0 {
                children.push(self.literal("?"));
            } else if ch == '?' {
                children.push(self.node(Node::Any));
                previous = 't';
            } else {
                children.push(self.literal(&ch.to_string()));
                previous = 't';
            }
            i += 1;
        }
        self.node(Node::Sequence(children))
    }

    fn class(&mut self, chars: &[char]) -> usize {
        let mut body: String = chars[1..chars.len() - 1].iter().collect();
        let posix = body.contains("[:");
        for (name, ranges) in [
            ("alnum", "a-zA-Z0-9"),
            ("alpha", "a-zA-Z"),
            ("ascii", "\0-\u{7f}"),
            ("blank", " \t"),
            ("cntrl", "\0-\u{1f}\u{7f}"),
            ("digit", "0-9"),
            ("graph", "!-~"),
            ("lower", "a-z"),
            ("print", " -~"),
            ("punct", "!-/:-@[-`{-~"),
            ("space", " \t\r\n\u{b}\u{c}"),
            ("upper", "A-Z"),
            ("word", "A-Za-z0-9_"),
            ("xdigit", "A-Fa-f0-9"),
        ] {
            body = body.replace(&format!("[:{name}:]"), ranges);
        }
        if body.contains('\\') {
            self.needs_regex = true;
            let mut expression = body.clone();
            if body.starts_with('^') && !body.contains('/') {
                expression.push('/');
            }
            let class = self.node(Node::Raw(format!("[{expression}]")));
            if !posix && !body.chars().any(|ch| "-*+?.^${}(|)[]".contains(ch)) {
                let mut literal = String::new();
                for ch in chars {
                    if "-*+?.^${}(|)[]".contains(*ch) {
                        literal.push('\\');
                    }
                    literal.push(*ch);
                }
                let literal = self.node(Node::Raw(literal));
                return self.node(Node::Alternative(vec![literal, class]));
            }
            return class;
        }
        let negated = body.starts_with('^');
        let units: Vec<_> = body
            .strip_prefix('^')
            .unwrap_or(&body)
            .encode_utf16()
            .collect();
        let mut ranges = Vec::new();
        let mut i = 0;
        while i < units.len() {
            let mut value = units[i];
            if value == b'\\' as u16 && i + 1 < units.len() {
                i += 1;
                value = units[i];
            }
            if units.get(i + 1) == Some(&(b'-' as u16)) && i + 2 < units.len() {
                if value > units[i + 2] {
                    return self.node(Node::Never);
                }
                ranges.push((value, units[i + 2]));
                i += 3;
            } else {
                ranges.push((value, value));
                i += 1;
            }
        }
        if negated && !body.contains('/') {
            ranges.push((b'/' as u16, b'/' as u16));
        }
        let class = self.node(Node::Class {
            ranges: merge_ranges(ranges),
            negated,
        });
        if !posix && !body.chars().any(|ch| "-*+?.^${}(|)[]".contains(ch)) {
            let literal = self.literal(&chars.iter().collect::<String>());
            self.node(Node::Alternative(vec![class, literal]))
        } else {
            class
        }
    }

    #[cfg(test)]
    fn is_match(&self, path: &str, work: &mut usize) -> Result<bool> {
        self.is_match_budgeted(path, work, 1)
    }
    fn is_match_budgeted(
        &self,
        path: &str,
        work: &mut usize,
        remaining_patterns: usize,
    ) -> Result<bool> {
        // Picomatch.test first accepts exact pattern strings, including malformed glob text.
        *work = work.saturating_add(path.len().min(self.raw.len()) + 1);
        ensure!(
            *work <= 2_000_000,
            "Snapshot pattern matching exceeded its work limit"
        );
        if path == self.raw {
            return Ok(true);
        }
        let units: Vec<_> = path.encode_utf16().collect();
        if let Some(regex) = &self.regex {
            return regex.is_match(&units, work, remaining_patterns);
        }
        if path.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
            return regex::Compiled::new(self)?.is_match(&units, work, remaining_patterns);
        }
        let mut evaluator = Evaluator {
            pattern: self,
            input: &units,
            memo: HashMap::new(),
            work: *work + units.len(),
        };
        let matched = evaluator.ends(self.root, 0)?.contains(&units.len());
        *work = evaluator.work;
        Ok(matched != self.negated)
    }
}

fn raw_group(body: &[char]) -> (String, usize) {
    // Picomatch tokenizes the header's '!' too: before '(' it opens an extglob and
    // produces an invalid JS group, rather than behaving like a regex parser alone.
    if body.starts_with(&['?', '!', '(']) {
        return ("?".into(), 1);
    }
    if body.starts_with(&['?', '<', '!', '(']) {
        return ("?<".into(), 2);
    }
    for prefix in ["?<=", "?<!", "?=", "?!", "?:"] {
        let chars: Vec<_> = prefix.chars().collect();
        if body.starts_with(&chars) {
            return (prefix.into(), chars.len());
        }
    }
    if body.starts_with(&['?', '<'])
        && let Some(end) = body.iter().position(|ch| *ch == '>')
        && end > 2
        && body[2..end]
            .iter()
            .all(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
    {
        return (body[..=end].iter().collect(), end + 1);
    }
    (String::new(), 0)
}

fn negative_extglob_start(text: &str) -> bool {
    let chars: Vec<_> = text.chars().take(4).collect();
    chars.starts_with(&['!', '('])
        && !(chars.get(2) == Some(&'?') && chars.get(3).is_some_and(|ch| "!=<:".contains(*ch)))
}

struct Evaluator<'a> {
    pattern: &'a Pattern,
    input: &'a [u16],
    memo: HashMap<(usize, usize), Vec<usize>>,
    work: usize,
}

impl Evaluator<'_> {
    fn charge(&mut self, amount: usize) -> Result<()> {
        self.work = self.work.saturating_add(amount);
        ensure!(
            self.work <= 2_000_000,
            "Snapshot pattern matching exceeded its work limit"
        );
        Ok(())
    }

    fn insert(&mut self, set: &mut BTreeSet<usize>, position: usize) -> Result<bool> {
        self.charge((usize::BITS - set.len().leading_zeros()) as usize + 1)?;
        Ok(set.insert(position))
    }

    fn ends(&mut self, id: usize, start: usize) -> Result<Vec<usize>> {
        if let Some(ends) = self.memo.get(&(id, start)) {
            // Charge before cloning: repeated memo hits must not bypass the allocation/work cap.
            self.work = self.work.saturating_add(ends.len() + 1);
            ensure!(
                self.work <= 2_000_000,
                "Snapshot pattern matching exceeded its work limit"
            );
            return Ok(ends.clone());
        }
        self.charge(1)?;
        let result = match &self.pattern.nodes[id] {
            Node::Never => Vec::new(),
            Node::Group { inner, .. } => self.ends(*inner, start)?,
            Node::Raw(_) => unreachable!("raw regex node uses the regex backend"),
            Node::Literal(text) => {
                self.charge(text.len().min(self.input.len() - start))?;
                if self.input[start..].starts_with(text) {
                    vec![start + text.len()]
                } else {
                    Vec::new()
                }
            }
            Node::Any => {
                if self.input.get(start).is_some_and(|ch| *ch != b'/' as u16) {
                    vec![start + 1]
                } else {
                    Vec::new()
                }
            }
            Node::Star { .. } => {
                let end = self.component_end(start);
                self.charge(end - start + 1)?;
                (start..=end).collect()
            }
            Node::Globstar => (start..=self.input.len()).collect(),
            Node::Directories => {
                self.charge(self.input.len() - start + 1)?;
                std::iter::once(start)
                    .chain(
                        (start..self.input.len())
                            .filter(|i| self.input[*i] == b'/' as u16)
                            .map(|i| i + 1),
                    )
                    .collect()
            }
            Node::Tail => {
                if self.input.get(start) == Some(&(b'/' as u16)) {
                    (start..=self.input.len()).collect()
                } else {
                    vec![start]
                }
            }
            Node::Class { ranges, negated } => {
                self.charge((usize::BITS - ranges.len().leading_zeros()) as usize + 1)?;
                match self.input.get(start) {
                    Some(value) => {
                        let index = ranges.partition_point(|(lo, _)| lo <= value);
                        let contained = index > 0 && *value <= ranges[index - 1].1;
                        if contained != *negated {
                            vec![start + 1]
                        } else {
                            Vec::new()
                        }
                    }
                    None => Vec::new(),
                }
            }
            Node::Sequence(children) => {
                let mut positions = vec![start];
                for child in children {
                    let mut next = BTreeSet::new();
                    for position in positions {
                        for end in self.ends(*child, position)? {
                            self.insert(&mut next, end)?;
                        }
                    }
                    self.charge(next.len())?;
                    positions = next.into_iter().collect();
                    if positions.is_empty() {
                        break;
                    }
                }
                positions
            }
            Node::Alternative(children) => {
                let mut positions = BTreeSet::new();
                for child in children {
                    for end in self.ends(*child, start)? {
                        self.insert(&mut positions, end)?;
                    }
                }
                positions.into_iter().collect()
            }
            Node::Repeat {
                inner,
                minimum,
                once,
            } => {
                let mut reached = if *minimum == 0 {
                    BTreeSet::from([start])
                } else {
                    BTreeSet::new()
                };
                let first = self.ends(*inner, start)?;
                for end in &first {
                    self.insert(&mut reached, *end)?;
                }
                if !once {
                    let mut pending = first;
                    while let Some(position) = pending.pop() {
                        for next in self.ends(*inner, position)? {
                            if self.insert(&mut reached, next)? {
                                self.charge(1)?;
                                pending.push(next);
                            }
                        }
                    }
                }
                reached.into_iter().collect()
            }
            Node::Negative {
                inner,
                anchored,
                slash,
            } => {
                let inner_ends = self.ends(*inner, start)?;
                self.charge(inner_ends.len())?;
                let blocked = inner_ends
                    .iter()
                    .any(|end| !anchored || *end == self.input.len());
                if blocked {
                    Vec::new()
                } else {
                    let end = if *slash {
                        self.input.len()
                    } else {
                        let end = self.component_end(start);
                        self.charge(end - start + 1)?;
                        end
                    };
                    (start..=end).collect()
                }
            }
        };
        // Account for generated positions and their cached copy, including scanning ranges.
        self.charge(result.len().saturating_mul(2) + 1)?;
        self.memo.insert((id, start), result.clone());
        Ok(result)
    }

    fn component_end(&self, start: usize) -> usize {
        self.input[start..]
            .iter()
            .position(|ch| *ch == b'/' as u16)
            .map_or(self.input.len(), |n| start + n)
    }
}

fn check_depth(raw: &str) -> Result<()> {
    let chars: Vec<_> = raw.chars().collect();
    let mut depth = 0usize;
    let mut i = 0;
    let mut quoted = false;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '"' => quoted = !quoted,
            '[' if !quoted => {
                if let Some(end) = bracket_end(&chars, i) {
                    i = end;
                }
            }
            '(' | '{' if !quoted => {
                depth += 1;
                ensure!(
                    depth <= 128,
                    "Snapshot pattern exceeds the native nesting safety limit of 128"
                );
            }
            ')' | '}' if !quoted => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

fn closing(text: &[char], open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0;
    let mut escaped = false;
    let mut quoted = false;
    let mut i = open;
    while i < text.len() {
        let ch = text[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
            i += 1;
            continue;
        }
        if quoted {
            i += 1;
            continue;
        }
        if ch == '['
            && left != '['
            && let Some(end) = bracket_end(text, i)
        {
            i = end + 1;
            continue;
        }
        if ch == left {
            depth += 1;
        }
        if ch == right {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn bracket_end(text: &[char], start: usize) -> Option<usize> {
    let mut i = start + 1;
    if text.get(i) == Some(&'^') {
        i += 1;
    }
    if text.get(i) == Some(&']') {
        i += 1;
    }
    while i < text.len() {
        if text[i] == '\\' {
            i += 2;
            continue;
        }
        if text[i] == '[' && text.get(i + 1) == Some(&':') {
            i += 2;
            while i + 1 < text.len() && !(text[i] == ':' && text[i + 1] == ']') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if text[i] == ']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn split(text: &[char], separator: char) -> Vec<&[char]> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < text.len() {
        if text[i] == '\\' {
            i += 2;
            continue;
        }
        let end = match text[i] {
            '(' => closing(text, i, '(', ')'),
            '{' => closing(text, i, '{', '}'),
            '[' => bracket_end(text, i),
            _ => None,
        };
        if let Some(end) = end {
            i = end + 1;
            continue;
        }
        if text[i] == separator {
            parts.push(&text[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    parts.push(&text[start..]);
    parts
}

fn merge_ranges(mut ranges: Vec<(u16, u16)>) -> Vec<(u16, u16)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u16, u16)> = Vec::new();
    for (lo, hi) in ranges {
        if let Some(last) = merged.last_mut()
            && lo <= last.1.saturating_add(1)
        {
            last.1 = last.1.max(hi);
        } else {
            merged.push((lo, hi));
        }
    }
    merged
}

enum RepeatAnalysis {
    Safe,
    Characters(Vec<u16>),
    Literal,
}

// Port of pinned picomatch4.0.7 analyzeRepeatedExtglob, with its default recursion allowance 0.
// The source intentionally literalizes risky nested repeats, except single-unit branches that
// it flattens to a character class followed by '*', including when the outer operator is '+'.
fn analyze_repeat(body: &[char]) -> RepeatAnalysis {
    let parts: Vec<String> = split(body, '|')
        .into_iter()
        .map(|part| part.iter().collect::<String>().trim().into())
        .collect();
    if parts.len() > 1 {
        if parts
            .iter()
            .any(|part| part.is_empty() || part.chars().all(|ch| matches!(ch, '*' | '?')))
        {
            return RepeatAnalysis::Literal;
        }
        let mut repeated = BTreeSet::new();
        for part in &parts {
            if let Some(literal) = simple_branch(part)
                && let Some(first) = literal.first()
                && literal.iter().all(|ch| ch == first)
                && !repeated.insert(*first)
            {
                return RepeatAnalysis::Literal;
            }
        }
    }
    let mut safe_chars = Vec::new();
    let mut saw_star_sequence = false;
    let mut combinable = true;
    for part in parts {
        if let Some(chars) = star_sequence(&part) {
            saw_star_sequence = true;
            safe_chars.extend(chars);
            continue;
        }
        if let Some(literal) = simple_branch(&part)
            && literal.len() == 1
        {
            safe_chars.extend(literal);
            continue;
        }
        combinable = false;
        let chars: Vec<_> = part.chars().collect();
        if repeated_group(&chars).is_some() {
            return RepeatAnalysis::Literal;
        }
    }
    if saw_star_sequence {
        if combinable {
            RepeatAnalysis::Characters(safe_chars)
        } else {
            RepeatAnalysis::Literal
        }
    } else {
        RepeatAnalysis::Safe
    }
}

fn repeated_group(chars: &[char]) -> Option<(char, &[char])> {
    if !matches!(chars.first(), Some('*' | '+')) || chars.get(1) != Some(&'(') {
        return None;
    }
    let close = closing(chars, 1, '(', ')')?;
    (close + 1 == chars.len()).then(|| (chars[0], &chars[2..close]))
}

fn simple_branch(branch: &str) -> Option<Vec<u16>> {
    let mut chars: Vec<_> = branch.trim().chars().collect();
    loop {
        if chars.starts_with(&['@', '('])
            && chars.last() == Some(&')')
            && !chars[2..chars.len() - 1]
                .iter()
                .any(|ch| "\\()[]{}|".contains(*ch))
        {
            chars = chars[2..chars.len() - 1].to_vec();
        } else {
            break;
        }
    }
    let mut literal = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 1;
            if let Some(ch) = chars.get(i) {
                literal.push(*ch);
            }
        } else if "?*+@!()[]{}".contains(chars[i]) {
            return None;
        } else {
            literal.push(chars[i]);
        }
        i += 1;
    }
    Some(literal.encode_utf16().collect())
}

fn star_sequence(branch: &str) -> Option<Vec<u16>> {
    let chars: Vec<_> = branch.chars().collect();
    let mut i = 0;
    let mut values = Vec::new();
    while i < chars.len() {
        if !chars[i..].starts_with(&['*', '(']) {
            return None;
        }
        let end = closing(&chars, i + 1, '(', ')')?;
        let body = &chars[i + 2..end];
        if split(body, '|').len() != 1 {
            return None;
        }
        let literal = simple_branch(&body.iter().collect::<String>())?;
        if literal.len() != 1 {
            return None;
        }
        values.extend(literal);
        i = end + 1;
    }
    (!values.is_empty()).then_some(values)
}

fn scan_base(raw: &str) -> String {
    let mut text = raw;
    loop {
        if let Some(rest) = text.strip_prefix("./") {
            text = rest;
        } else if text.starts_with('!') && !text.starts_with("!(") {
            text = &text[1..];
        } else {
            break;
        }
    }
    let chars: Vec<_> = text.chars().collect();
    let mut last_slash = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '/' => last_slash = i + 1,
            '*' | '?' | '(' => return chars[..last_slash.saturating_sub(1)].iter().collect(),
            '@' | '+' | '!' if chars.get(i + 1) == Some(&'(') => {
                return chars[..last_slash.saturating_sub(1)].iter().collect();
            }
            '[' if bracket_end(&chars, i).is_some() => {
                return chars[..last_slash.saturating_sub(1)].iter().collect();
            }
            '{' => {
                if let Some(close) = closing(&chars, i, '{', '}') {
                    let body: String = chars[i + 1..close].iter().collect();
                    if body.contains(',') || body.contains("..") {
                        return chars[..last_slash.saturating_sub(1)].iter().collect();
                    }
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    text.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_result_copies_are_charged_before_allocation() {
        let pattern = Pattern::new("*").unwrap();
        let input = vec![b'a' as u16; 1000];
        let mut evaluator = Evaluator {
            pattern: &pattern,
            input: &input,
            memo: HashMap::new(),
            work: 0,
        };
        evaluator.ends(pattern.root, 0).unwrap();
        let mut exhausted = false;
        for _ in 0..2500 {
            if let Err(error) = evaluator.ends(pattern.root, 0) {
                assert!(error.to_string().contains("work limit"));
                exhausted = true;
                break;
            }
        }
        assert!(exhausted, "memo-hit copies cannot bypass the work budget");
    }

    #[test]
    fn large_duplicate_character_class_is_merged_before_matching() {
        let text = format!("*([{}a])", "b".repeat(65_520));
        let pattern = Pattern::new(&text).unwrap();
        assert!(
            pattern
                .nodes
                .iter()
                .filter_map(|node| match node {
                    Node::Class { ranges, .. } => Some(ranges.len()),
                    _ => None,
                })
                .all(|count| count == 1)
        );
        assert!(pattern.is_match(&"a".repeat(255), &mut 0).unwrap());
    }
}
