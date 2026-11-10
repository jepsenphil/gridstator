//! Commander option/operand ordering at the compatibility boundary. Clap executes the resulting
//! registered flags; this pass preserves deferred unknowns, short clusters and required values.
use serde_json::Value;
use std::collections::HashSet;
pub(super) enum Parsed {
    Arguments(Vec<String>),
    Help { path: Vec<String>, error: bool },
    Version,
    Error(String),
    Native,
}
fn error(message: String) -> Parsed {
    Parsed::Error(format!("error: {message}\n(run with --help for usage)\n"))
}
fn names(option: &Value) -> Vec<&str> {
    option["flags"]
        .as_str()
        .unwrap()
        .split(',')
        .filter_map(|flag| flag.split_whitespace().next())
        .collect()
}
fn children<'a>(case: &Value, catalog: &'a [Value]) -> Vec<&'a Value> {
    let path = case["path"].as_array().unwrap();
    catalog
        .iter()
        .filter(|child| {
            let other = child["path"].as_array().unwrap();
            other.len() == path.len() + 1 && other[..path.len()] == path[..]
        })
        .collect()
}
fn name(case: &Value) -> &str {
    case["path"]
        .as_array()
        .unwrap()
        .last()
        .and_then(Value::as_str)
        .unwrap_or("gridstator")
}
fn path(case: &Value) -> Vec<String> {
    case["path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}
fn find_child<'a>(case: &Value, catalog: &'a [Value], word: &str) -> Option<&'a Value> {
    children(case, catalog).into_iter().find(|child| {
        name(child) == word
            || child["aliases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|alias| alias == word)
    })
}
struct Frame {
    operands: Vec<String>,
    unknown: Vec<String>,
    flags: Vec<String>,
    seen: HashSet<String>,
}
fn options(case: &Value) -> Vec<&Value> {
    case["options"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|option| {
            !(case["path"] == serde_json::json!(["agents", "set"])
                && names(option).contains(&"--node"))
        })
        .collect()
}
fn scan(case: &Value, args: Vec<String>) -> Result<Frame, Parsed> {
    let options = options(case);
    let mut frame = Frame {
        operands: vec![],
        unknown: vec![],
        flags: vec![],
        seen: HashSet::new(),
    };
    let mut args = std::collections::VecDeque::from(args);
    let mut unknown = false;
    let mut variadic: Option<&Value> = None;
    while let Some(arg) = args.pop_front() {
        if arg == "--" {
            if unknown {
                frame.unknown.push(arg);
                frame.unknown.extend(args);
            } else {
                frame.operands.extend(args);
            }
            break;
        }
        let maybe_option = arg.starts_with('-') && arg.len() > 1;
        if let Some(option) = variadic
            && !maybe_option
        {
            frame.flags.push(format!(
                "{}={arg}",
                names(option)
                    .into_iter()
                    .find(|name| name.starts_with("--"))
                    .unwrap()
            ));
            continue;
        }
        variadic = None;
        let exact = options
            .iter()
            .copied()
            .find(|option| names(option).contains(&arg.as_str()));
        let mut attached = None;
        let option = if let Some(option) = exact {
            Some(option)
        } else if arg.starts_with("--") {
            arg.split_once('=').and_then(|(flag, value)| {
                options
                    .iter()
                    .copied()
                    .find(|option| {
                        names(option).contains(&flag)
                            && (option["required"] == true || option["optional"] == true)
                    })
                    .inspect(|_| attached = Some(value.to_owned()))
            })
        } else if arg.starts_with('-') && arg.len() > 2 {
            let end = 1 + arg[1..].chars().next().unwrap().len_utf8();
            let flag = &arg[..end];
            options
                .iter()
                .copied()
                .find(|option| names(option).contains(&flag))
                .inspect(|option| {
                    if option["required"] == true || option["optional"] == true {
                        attached = Some(arg[end..].to_owned());
                    } else {
                        args.push_front(format!("-{}", &arg[end..]));
                    }
                })
        } else {
            None
        };
        if let Some(option) = option {
            let flag = names(option)
                .into_iter()
                .find(|name| name.starts_with("--"))
                .unwrap();
            if flag == "--version" {
                return Err(Parsed::Version);
            }
            frame.seen.insert(flag.to_owned());
            if option["required"] == true {
                let Some(value) = attached.or_else(|| args.pop_front()) else {
                    return Err(error(format!(
                        "option '{}' argument missing",
                        option["flags"].as_str().unwrap()
                    )));
                };
                frame.flags.push(format!("{flag}={value}"));
                if option["variadic"] == true {
                    variadic = Some(option);
                }
            } else if option["optional"] == true {
                let value = attached.or_else(|| {
                    args.front()
                        .filter(|value| !value.starts_with('-'))
                        .cloned()
                        .inspect(|_| {
                            args.pop_front();
                        })
                });
                frame.flags.push(
                    value
                        .map(|value| format!("{flag}={value}"))
                        .unwrap_or_else(|| flag.to_owned()),
                );
            } else {
                frame.flags.push(flag.to_owned());
            }
        } else if maybe_option {
            unknown = true;
            frame.unknown.push(arg);
        } else if unknown {
            frame.unknown.push(arg);
        } else {
            frame.operands.push(arg);
        }
    }
    Ok(frame)
}
fn mandatory(case: &Value, seen: &HashSet<String>) -> Option<Parsed> {
    options(case)
        .iter()
        .find(|option| {
            option["mandatory"] == true
                && option.get("default").is_none()
                && !seen.contains(
                    names(option)
                        .into_iter()
                        .find(|name| name.starts_with("--"))
                        .unwrap(),
                )
        })
        .map(|option| {
            error(format!(
                "required option '{}' not specified",
                option["flags"].as_str().unwrap()
            ))
        })
}
fn unknown_option(case: &Value, catalog: &[Value], flag: &str) -> Parsed {
    let current = path(case);
    let mut candidates = vec!["--help".to_owned()];
    for ancestor in catalog.iter().filter(|ancestor| {
        let p = path(ancestor);
        p.len() <= current.len() && current[..p.len()] == p[..]
    }) {
        for option in options(ancestor) {
            candidates.extend(
                names(option)
                    .into_iter()
                    .filter(|name| name.starts_with("--"))
                    .map(str::to_owned),
            );
        }
    }
    error(format!(
        "unknown option '{flag}'{}",
        super::presentation::suggestion(flag, candidates)
    ))
}
fn frame(
    case: &Value,
    catalog: &[Value],
    incoming: Vec<String>,
    args: Vec<String>,
    mut prefix: Vec<String>,
) -> Parsed {
    let mut parsed = match scan(case, args) {
        Ok(parsed) => parsed,
        Err(outcome) => return outcome,
    };
    let mut operands = incoming;
    operands.append(&mut parsed.operands);
    if let Some(word) = operands.first() {
        if let Some(child) = find_child(case, catalog, word) {
            prefix.extend(parsed.flags);
            prefix.push(name(child).to_owned());
            return frame(
                child,
                catalog,
                operands[1..].to_vec(),
                parsed.unknown,
                prefix,
            );
        }
        if word == "help" && case["implicitHelp"] == true {
            return match operands.get(1) {
                None => Parsed::Help {
                    path: path(case),
                    error: false,
                },
                Some(word) => match find_child(case, catalog, word) {
                    Some(child) => Parsed::Help {
                        path: path(child),
                        error: false,
                    },
                    None => error(format!(
                        "unknown command '{word}'{}",
                        super::presentation::suggestion(
                            word,
                            children(case, catalog)
                                .iter()
                                .map(|child| name(child).to_owned())
                                .collect()
                        )
                    )),
                },
            };
        }
        if case["path"].as_array().unwrap().is_empty()
            && [
                "build-info",
                "discover",
                "reports",
                "release",
                "bridge",
                "supervise",
            ]
            .contains(&word.as_str())
        {
            return Parsed::Native;
        }
    }
    let children = children(case, catalog);
    if !children.is_empty()
        && operands.is_empty()
        && parsed.unknown.is_empty()
        && case["hasAction"] != true
    {
        return Parsed::Help {
            path: path(case),
            error: true,
        };
    }
    if parsed
        .unknown
        .iter()
        .any(|arg| arg == "--help" || arg == "-h")
    {
        return Parsed::Help {
            path: path(case),
            error: false,
        };
    }
    if let Some(error) = mandatory(case, &parsed.seen) {
        return error;
    }
    if case["hasAction"] != true && !children.is_empty() && !operands.is_empty() {
        let word = &operands[0];
        let mut candidates = children
            .iter()
            .map(|child| name(child).to_owned())
            .collect::<Vec<_>>();
        if case["implicitHelp"] == true {
            candidates.push("help".into());
        }
        return error(format!(
            "unknown command '{word}'{}",
            super::presentation::suggestion(word, candidates)
        ));
    }
    if let Some(flag) = parsed.unknown.first() {
        return unknown_option(case, catalog, flag);
    }
    let arguments = case["arguments"].as_array().unwrap();
    for (index, argument) in arguments.iter().enumerate() {
        if argument["required"] == true && operands.len() <= index {
            return error(format!(
                "missing required argument '{}'",
                argument["name"].as_str().unwrap()
            ));
        }
    }
    if !arguments
        .iter()
        .any(|argument| argument["variadic"] == true)
        && operands.len() > arguments.len()
    {
        let expected = arguments.len();
        let suffix = if expected == 1 { "" } else { "s" };
        let command = if path(case).is_empty() {
            String::new()
        } else {
            format!(" for '{}'", name(case))
        };
        return error(format!(
            "too many arguments{command}. Expected {expected} argument{suffix} but got {}: {}.",
            operands.len(),
            operands.join(", ")
        ));
    }
    if !children.is_empty() && case["hasAction"] != true {
        return Parsed::Help {
            path: path(case),
            error: true,
        };
    }
    prefix.extend(parsed.flags);
    if !operands.is_empty() {
        prefix.push("--".into());
        prefix.extend(operands);
    }
    Parsed::Arguments(prefix)
}
pub(super) fn arguments(args: &[String], catalog: &[Value]) -> Parsed {
    frame(&catalog[0], catalog, vec![], args.to_vec(), vec![])
}
