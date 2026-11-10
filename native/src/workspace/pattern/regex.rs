//! ECMAScript assertions/capture semantics, executed over UCS-2 without a JavaScript runtime.
use super::{Node, Pattern};
use anyhow::{Result, ensure};

const LIMIT: usize = 2_000_000;

#[derive(Clone)]
pub(super) struct Compiled {
    regex: Option<regress::Regex>,
    program_size: usize,
    captures: usize,
}

impl Compiled {
    pub(super) fn new(pattern: &Pattern) -> Result<Self> {
        let mut emitter = Emitter {
            pattern,
            output: Vec::new(),
            visits: 0,
            captures: 0,
        };
        emitter.text(if pattern.negated { "^(?!^(?:" } else { "^(?:" });
        emitter.node(pattern.root)?;
        emitter.text(if pattern.negated { ")$).*$" } else { ")$" });
        ensure!(
            emitter.output.len() <= LIMIT,
            "Snapshot regex exceeds the native compiled-size safety limit"
        );
        // Disable optimizer rewrites: compilation remains proportional to bounded syntax rather
        // than attempting combinatorial transformations of nested expressions.
        let flags = regress::Flags {
            no_opt: true,
            ..Default::default()
        };
        let program_size = emitter.output.len();
        let captures = emitter.captures;
        // Picomatch's default toRegex catches invalid JS regex syntax and returns /$^/. The
        // exact-pattern-string shortcut remains in Pattern::is_match before this backend.
        let regex = regress::Regex::from_unicode(emitter.output.into_iter(), flags).ok();
        Ok(Self {
            regex,
            program_size,
            captures,
        })
    }

    pub(super) fn is_match(
        &self,
        input: &[u16],
        work: &mut usize,
        remaining: usize,
    ) -> Result<bool> {
        let Some(regex) = &self.regex else {
            return Ok(false);
        };
        // Upstream fuel counts backtrack events, not instructions: restoring a capture-heavy
        // stack and replaying a long forward program can cost much more than one event. Weight
        // each allowance by the emitted program, capture slots and subject length. This is a
        // conservative complexity reservation, not a claim to count engine instructions.
        let event_cost = self
            .program_size
            .saturating_add(self.captures.saturating_mul(2))
            .saturating_add(1)
            .saturating_mul(input.len().saturating_add(1));
        *work = work.saturating_add(event_cost);
        ensure!(
            *work < LIMIT,
            "Snapshot pattern matching exceeded its work limit"
        );
        // The engine API reports exhaustion rather than remaining fuel. Reserve a fair share of
        // the remaining list budget; every regex call is charged its full reservation, so multiple
        // regex patterns cannot each consume a fresh unrestricted per-path budget.
        let reservation = (LIMIT - *work) / remaining.max(1);
        let budget = reservation / event_cost;
        ensure!(
            budget > 0,
            "Snapshot pattern matching exceeded its work limit"
        );
        *work += reservation;
        match regex.find_from_ucs2_budgeted(input, 0, budget as u64) {
            regress::BudgetedMatch::Match(_) => Ok(true),
            regress::BudgetedMatch::NoMatch => Ok(false),
            regress::BudgetedMatch::BudgetExhausted => {
                anyhow::bail!("Snapshot pattern matching exceeded its work limit")
            }
        }
    }
}

struct Emitter<'a> {
    pattern: &'a Pattern,
    output: Vec<u32>,
    visits: usize,
    captures: usize,
}

impl Emitter<'_> {
    fn text(&mut self, text: &str) {
        self.output.extend(text.chars().map(u32::from));
    }

    fn literal(&mut self, units: &[u16]) {
        for unit in units {
            if *unit < 128 && "\\.^$|?*+()[]{}".contains(char::from_u32(u32::from(*unit)).unwrap())
            {
                self.text("\\");
            }
            self.output.push(u32::from(*unit));
        }
    }

    fn class(&mut self, ranges: &[(u16, u16)], negated: bool) {
        if negated {
            self.text("[^");
        } else {
            self.text("[");
        }
        for (lo, hi) in ranges {
            for (index, unit) in [*lo, *hi].into_iter().enumerate() {
                if index == 1 {
                    if lo == hi {
                        break;
                    }
                    self.text("-");
                }
                if unit < 128 && "\\][^-".contains(char::from_u32(u32::from(unit)).unwrap()) {
                    self.text("\\");
                }
                self.output.push(u32::from(unit));
            }
        }
        self.text("]");
    }

    fn node(&mut self, id: usize) -> Result<()> {
        self.visits += 1;
        ensure!(
            self.visits <= 200_000 && self.output.len() <= LIMIT,
            "Snapshot regex exceeds the native compiled-size safety limit"
        );
        match &self.pattern.nodes[id] {
            Node::Never => self.text("(?!)"),
            Node::Literal(text) => self.literal(text),
            Node::Raw(text) => self.raw(text),
            Node::Group { prefix, inner } => {
                if prefix.is_empty()
                    || (prefix.starts_with("?<")
                        && !prefix.starts_with("?<=")
                        && !prefix.starts_with("?<!"))
                {
                    self.captures += 1;
                }
                ensure!(
                    self.captures <= 4096,
                    "Snapshot regex exceeds the native capture-count safety limit"
                );
                self.text("(");
                self.text(prefix);
                self.node(*inner)?;
                self.text(")");
            }
            Node::Any => self.text("[^/]"),
            Node::Star { component_start } => self.text(if *component_start {
                "(?=.)[^/]*?"
            } else {
                "[^/]*?"
            }),
            Node::Globstar => self.text("(?:(?!(?:^|/)\\.{1,2}(?:/|$)).)*?"),
            Node::Directories => self.text("(?:(?:(?!(?:^|/)\\.{1,2}(?:/|$)).)*?/)?"),
            Node::Tail => self.text("(?:/(?:(?!(?:^|/)\\.{1,2}(?:/|$)).)*?|)"),
            Node::Class { ranges, negated } => self.class(ranges, *negated),
            Node::Sequence(children) => {
                for child in children {
                    self.node(*child)?;
                }
            }
            Node::Alternative(children) => {
                self.text("(?:");
                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        self.text("|");
                    }
                    self.node(*child)?;
                }
                self.text(")");
            }
            Node::Repeat {
                inner,
                minimum,
                once,
            } => {
                self.text("(?:");
                self.node(*inner)?;
                self.text(")");
                self.text(if *once {
                    "?"
                } else if *minimum > 0 {
                    "+"
                } else {
                    "*"
                });
            }
            Node::Negative {
                inner,
                anchored,
                slash,
            } => {
                self.text("(?:(?!(?:");
                self.node(*inner)?;
                self.text(if *anchored { ")$)" } else { "))" });
                self.text(if *slash {
                    "(?:(?!(?:^|/)\\.{1,2}(?:/|$)).)*?)"
                } else {
                    "[^/]*?)"
                });
            }
        }
        Ok(())
    }
}

impl Emitter<'_> {
    fn raw(&mut self, text: &str) {
        if text.starts_with("\\k<") {
            self.text(text);
            return;
        }
        let chars: Vec<_> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\\' && chars.get(i + 1) == Some(&'\\') {
                self.text("\\\\");
                i += 2;
                continue;
            }
            if chars[i] == '\\'
                && chars.get(i + 1) == Some(&'u')
                && let Some(digits) = chars.get(i + 2..i + 6)
                && digits.iter().all(char::is_ascii_hexdigit)
                && let Ok(unit) = u16::from_str_radix(&digits.iter().collect::<String>(), 16)
                && (0xd800..=0xdfff).contains(&unit)
            {
                // The engine's escape parser combines adjacent surrogate escapes even in
                // non-Unicode mode. Passing literal UCS-2 units keeps JS's separate-unit class
                // and backreference behavior without changing named identifier parsing.
                self.output.push(u32::from(unit));
                i += 6;
                continue;
            }
            self.output.push(u32::from(chars[i]));
            i += 1;
        }
    }
}
