//! Bounded in-memory JUnit parsing and report extraction; archive contents are never materialized.
use anyhow::{Context, Result, bail, ensure};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde_json::{Map, Number, Value, json};
use std::io::{Cursor, Read};

pub const MAX_REPORT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_XML_DEPTH: usize = 128;
const MAX_XML_NODES: usize = 200_000;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;

#[derive(Default)]
struct Element {
    name: String,
    attributes: Map<String, Value>,
    children: Vec<Element>,
    text: String,
}

fn element(start: &BytesStart<'_>, reader: &Reader<&[u8]>) -> Result<Element> {
    let mut attributes = Map::new();
    for attribute in start.attributes() {
        let attribute = attribute?;
        attributes.insert(
            String::from_utf8(attribute.key.as_ref().to_vec())?,
            json!(attribute.decode_and_unescape_value(reader.decoder())?),
        );
    }
    Ok(Element {
        name: String::from_utf8(start.name().as_ref().to_vec())?,
        attributes,
        ..Default::default()
    })
}

fn parse_xml(xml: &str) -> Result<Element> {
    ensure!(
        xml.len() as u64 <= MAX_REPORT_BYTES,
        "JUnit XML exceeds report size limit"
    );
    let mut reader = Reader::from_str(xml);
    let mut stack = vec![Element::default()];
    let mut nodes = 0;
    loop {
        match reader.read_event().context("Invalid JUnit XML")? {
            Event::Start(start) => {
                nodes += 1;
                ensure!(
                    nodes <= MAX_XML_NODES && stack.len() <= MAX_XML_DEPTH,
                    "JUnit XML exceeds structural limits"
                );
                stack.push(element(&start, &reader)?);
            }
            Event::Empty(start) => {
                nodes += 1;
                ensure!(nodes <= MAX_XML_NODES, "JUnit XML exceeds node limit");
                stack
                    .last_mut()
                    .unwrap()
                    .children
                    .push(element(&start, &reader)?);
            }
            Event::End(_) => {
                ensure!(stack.len() > 1, "Unmatched JUnit XML closing tag");
                let finished = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(finished);
            }
            Event::Text(text) => stack
                .last_mut()
                .unwrap()
                .text
                .push_str(&text.xml_content()?),
            Event::CData(text) => stack
                .last_mut()
                .unwrap()
                .text
                .push_str(&text.xml_content()?),
            Event::GeneralRef(reference) => {
                if let Some(character) = reference.resolve_char_ref()? {
                    stack.last_mut().unwrap().text.push(character);
                } else {
                    let name = reference.decode()?;
                    let value = quick_xml::escape::resolve_predefined_entity(&name)
                        .with_context(|| format!("Unknown XML entity &{name};"))?;
                    stack.last_mut().unwrap().text.push_str(value);
                }
            }
            Event::DocType(_) => bail!("DTD declarations are not supported in JUnit reports"),
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(stack.len() == 1, "Unclosed JUnit XML element");
    Ok(stack.pop().unwrap())
}

fn json_number(number: f64) -> Value {
    if number.is_finite() && number.fract() == 0.0 && number >= 0.0 && number < u64::MAX as f64 {
        return json!(number as u64);
    }
    if number.is_finite()
        && number.fract() == 0.0
        && number >= i64::MIN as f64
        && number < i64::MAX as f64
    {
        return json!(number as i64);
    }
    Number::from_f64(number)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn js_number(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0.0);
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok().map(|value| value as f64);
    }
    text.parse::<f64>().ok().filter(|number| !number.is_nan())
}

fn test_case(case: &Element, suite: &str) -> Value {
    let failure = case
        .children
        .iter()
        .find(|child| child.name == "failure")
        .or_else(|| case.children.iter().find(|child| child.name == "error"));
    let skipped = case.children.iter().any(|child| child.name == "skipped");
    let mut result = json!({"suite":suite,"name":case.attributes.get("name").and_then(Value::as_str).unwrap_or("test"),"status":if failure.is_some() { "failed" } else if skipped { "skipped" } else { "passed" }});
    for field in ["classname", "file"] {
        if let Some(value) = case
            .attributes
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            result[field] = json!(value);
        }
    }
    if let Some(line) = case
        .attributes
        .get("line")
        .and_then(Value::as_str)
        .filter(|line| !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()))
        && let Some(number) = js_number(line)
    {
        result["line"] = json_number(number);
    }
    if let Some(time) = case
        .attributes
        .get("time")
        .and_then(Value::as_str)
        .filter(|time| !time.is_empty())
        && let Some(number) = js_number(time)
    {
        result["timeMs"] = json_number((number * 1000.0 + 0.5).floor());
    }
    if let Some(failure) = failure {
        let text = failure.text.trim();
        let plain_text = failure.attributes.is_empty() && failure.children.is_empty();
        let numeric_text = !text.is_empty() && js_number(text).is_some();
        let boolean_text = text == "true" || text == "false";
        let message = failure
            .attributes
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if plain_text && !numeric_text && !boolean_text {
                    text.split('\n').next().unwrap_or("").into()
                } else {
                    "failed".into()
                }
            });
        result["message"] = json!(message);
        result["details"] = json!(if plain_text && (numeric_text || boolean_text) {
            ""
        } else {
            text
        });
    }
    result
}

fn visit_suite(suite: &Element, parents: &[String], cases: &mut Vec<Value>) {
    let name = suite
        .attributes
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("tests");
    let mut path = parents.to_vec();
    path.push(name.into());
    for inner in suite
        .children
        .iter()
        .filter(|child| child.name == "testsuite")
    {
        visit_suite(inner, &path, cases);
    }
    for case in suite
        .children
        .iter()
        .filter(|child| child.name == "testcase")
    {
        cases.push(test_case(case, &path.join(" › ")));
    }
}

/// Return the same array-of-cases JSON consumed by the v1 CLI and VS Code adapters.
pub fn parse_junit(xml: &str) -> Result<Value> {
    let document = parse_xml(xml)?;
    let mut cases = Vec::new();
    if let Some(root) = document
        .children
        .iter()
        .find(|element| element.name == "testsuites")
    {
        // Node's reporter writes direct cases here. Their suite names come from file basenames.
        for case in root
            .children
            .iter()
            .filter(|child| child.name == "testcase")
        {
            let name = case
                .attributes
                .get("file")
                .and_then(Value::as_str)
                .filter(|file| !file.is_empty())
                .map(|file| file.rsplit('/').next().unwrap_or(file))
                .unwrap_or("tests");
            cases.push(test_case(case, name));
        }
        for suite in root
            .children
            .iter()
            .filter(|child| child.name == "testsuite")
        {
            visit_suite(suite, &[], &mut cases);
        }
    }
    for suite in document
        .children
        .iter()
        .filter(|element| element.name == "testsuite")
    {
        visit_suite(suite, &[], &mut cases);
    }
    Ok(json!(cases))
}

pub fn looks_like_junit(text: &str) -> bool {
    let prefix = text.chars().take(4096).collect::<String>();
    regex::Regex::new(r"<testsuites?[\s>]")
        .expect("JUnit prefix regex")
        .is_match(&prefix)
}

pub fn candidate_reports(artifacts: &[Value]) -> Vec<Value> {
    artifacts
        .iter()
        .filter(|artifact| {
            let Some(name) = artifact["name"].as_str() else {
                return false;
            };
            artifact["size"]
                .as_u64()
                .is_some_and(|size| size <= MAX_REPORT_BYTES)
                && name != "image.tar"
                && [".xml", ".tar", ".zip"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
        })
        .cloned()
        .collect()
}

fn bounded_read(reader: impl Read, remaining: &mut u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(*remaining + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= *remaining,
        "Archive expands beyond report size limit"
    );
    *remaining -= bytes.len() as u64;
    Ok(bytes)
}

pub fn xml_documents(name: &str, bytes: &[u8]) -> Result<Vec<String>> {
    if ![".xml", ".tar", ".zip"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return Ok(Vec::new());
    }
    ensure!(
        bytes.len() as u64 <= MAX_REPORT_BYTES,
        "Artifact exceeds report size limit"
    );
    if name.ends_with(".xml") {
        return Ok(vec![String::from_utf8_lossy(bytes).into_owned()]);
    }
    let mut documents = Vec::new();
    let mut remaining = MAX_REPORT_BYTES;
    if name.ends_with(".zip") {
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).context("Invalid report ZIP archive")?;
        ensure!(
            archive.len() <= MAX_ARCHIVE_ENTRIES,
            "Too many report archive entries"
        );
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            if entry.is_dir() {
                continue;
            }
            ensure!(
                entry.size() <= remaining,
                "Archive expands beyond report size limit"
            );
            let xml = entry.name().ends_with(".xml");
            let content = bounded_read(entry, &mut remaining)?;
            if xml {
                documents.push(String::from_utf8_lossy(&content).into_owned());
            }
        }
    } else {
        let mut archive = tar::Archive::new(Cursor::new(bytes));
        for (index, entry) in archive.entries()?.enumerate() {
            ensure!(
                index < MAX_ARCHIVE_ENTRIES,
                "Too many report archive entries"
            );
            let entry = entry.context("Invalid report TAR entry")?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            ensure!(
                entry.size() <= remaining,
                "Archive expands beyond report size limit"
            );
            let xml = entry.path_bytes().ends_with(b".xml");
            let content = bounded_read(entry, &mut remaining)?;
            if xml {
                documents.push(String::from_utf8_lossy(&content).into_owned());
            }
        }
    }
    Ok(documents)
}

/// Parse every JUnit document in one already-downloaded artifact.
pub fn collect_junit(name: &str, bytes: &[u8]) -> Result<Value> {
    let mut cases = Vec::new();
    for xml in xml_documents(name, bytes)? {
        if looks_like_junit(&xml) {
            cases.extend(parse_junit(&xml)?.as_array().unwrap().iter().cloned());
        }
    }
    Ok(json!(cases))
}

/// Each set has `{cases: [...], shard?: number}`; failures retain the optional shard identity.
pub fn summarize(sets: &[Value]) -> Result<Value> {
    let mut summary = json!({"total":0,"passed":0,"failed":0,"skipped":0,"timeMs":0,"failures":[]});
    let mut counts = [0u64; 4];
    let mut time = 0.0;
    let mut failures = Vec::new();
    for set in sets {
        for case in set["cases"]
            .as_array()
            .context("JUnit summary set requires cases array")?
        {
            counts[0] += 1;
            time += case["timeMs"].as_f64().unwrap_or(0.0);
            match case["status"].as_str() {
                Some("passed") => counts[1] += 1,
                Some("skipped") => counts[3] += 1,
                _ => {
                    counts[2] += 1;
                    let mut failure = json!({"suite":case["suite"],"name":case["name"]});
                    if case["message"]
                        .as_str()
                        .is_some_and(|message| !message.is_empty())
                    {
                        failure["message"] = case["message"].clone();
                    }
                    if let Some(shard) = set.get("shard") {
                        failure["shard"] = shard.clone();
                    }
                    failures.push(failure);
                }
            }
        }
    }
    for (index, name) in ["total", "passed", "failed", "skipped"].iter().enumerate() {
        summary[*name] = json!(counts[index]);
    }
    summary["timeMs"] = json_number(time);
    summary["failures"] = json!(failures);
    Ok(summary)
}
