use gridstator::reports::*;
use serde_json::json;
use std::io::{Cursor, Write};
const REPORT: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<testsuites>
  <testsuite name="math" tests="3" failures="1" skipped="1">
    <testcase name="adds" classname="test" time="0.0012" file="/workspace/test/math.test.js" line="4"/>
    <testcase name="subtracts" classname="test" time="0.002">
      <failure message="Expected 1 to equal 2" type="AssertionError">AssertionError: Expected 1 to equal 2
    at TestContext.&lt;anonymous&gt; (/workspace/test/math.test.js:9:10)</failure>
    </testcase>
    <testcase name="later" classname="test"><skipped/></testcase>
    <testsuite name="nested"><testcase name="deep" time="0.5"/></testsuite>
  </testsuite>
</testsuites>"#;
#[test]
fn nested_suites_failures_entities_files_and_durations_match_reference() {
    assert!(looks_like_junit(REPORT));
    assert!(!looks_like_junit("<html/>"));
    let cases = parse_junit(REPORT).unwrap();
    assert_eq!(
        cases[0],
        json!({"suite":"math › nested","name":"deep","status":"passed","timeMs":500})
    );
    assert_eq!(
        cases[1],
        json!({"suite":"math","name":"adds","status":"passed","classname":"test","timeMs":1,"file":"/workspace/test/math.test.js","line":4})
    );
    assert_eq!(cases[2]["message"], "Expected 1 to equal 2");
    assert!(
        cases[2]["details"]
            .as_str()
            .unwrap()
            .contains("/workspace/test/math.test.js:9:10")
    );
    assert_eq!(
        cases
            .as_array()
            .unwrap()
            .iter()
            .map(|case| (
                case["suite"].as_str().unwrap(),
                case["name"].as_str().unwrap(),
                case["status"].as_str().unwrap(),
                case["timeMs"].as_i64()
            ))
            .collect::<Vec<_>>(),
        [
            ("math › nested", "deep", "passed", Some(500)),
            ("math", "adds", "passed", Some(1)),
            ("math", "subtracts", "failed", Some(2)),
            ("math", "later", "skipped", None)
        ]
    );
    assert_eq!(cases[3]["status"], "skipped");
    assert!(cases[3].get("timeMs").is_none());
}
#[test]
fn direct_cases_precede_suites_and_use_file_names() {
    let cases=parse_junit(r#"<testsuites><testsuite name="nested"><testcase name="deep"/></testsuite><testcase name="add" time="0.000880" file="/workspace/math.test.js"/><testcase name="broken" file="/workspace/broken.test.js"><failure message="boom">AssertionError</failure></testcase></testsuites>"#).unwrap();
    assert_eq!(cases[0]["suite"], "math.test.js");
    assert_eq!(cases[0]["timeMs"], 1);
    assert_eq!(cases[1]["suite"], "broken.test.js");
    assert_eq!(cases[1]["status"], "failed");
    assert_eq!(cases[2]["suite"], "nested");
    assert_eq!(
        parse_junit("<testsuite name=\"s\"><testcase name=\"t\"/></testsuite>").unwrap(),
        json!([{"suite":"s","name":"t","status":"passed"}])
    );
}
#[test]
fn failure_precedence_cdata_and_negative_rounding_are_preserved() {
    let cases=parse_junit("<testsuite><testcase name=\"x\" time=\"-0.0005\" line=\"-1\"><error message=\"error\"/><failure><![CDATA[first\nsecond]]></failure><skipped/></testcase><testcase><failure/></testcase></testsuite>").unwrap();
    assert_eq!(cases[0]["message"], "first");
    assert_eq!(cases[0]["details"], "first\nsecond");
    assert_eq!(cases[0]["status"], "failed");
    assert_eq!(cases[0]["timeMs"], 0);
    assert!(cases[0].get("line").is_none());
    assert_eq!(cases[1]["message"], "");
    assert_eq!(cases[1]["details"], "");
}
#[test]
fn raw_tar_zip_extraction_and_collection_use_xml_files() {
    assert_eq!(
        xml_documents("junit.xml", b"<testsuite/>").unwrap(),
        vec!["<testsuite/>"]
    );
    let mut tar = tar::Builder::new(Vec::new());
    for (name, content) in [("reports/junit.xml", REPORT), ("summary.txt", "ignored")] {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, content.as_bytes())
            .unwrap();
    }
    let tar = tar.into_inner().unwrap();
    assert_eq!(xml_documents("report.xml.tar", &tar).unwrap(), vec![REPORT]);
    assert_eq!(
        collect_junit("report.tar", &tar)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("report/junit.xml", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(REPORT.as_bytes()).unwrap();
    zip.start_file("summary.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"ignored").unwrap();
    let zip = zip.finish().unwrap().into_inner();
    assert_eq!(xml_documents("report.zip", &zip).unwrap(), vec![REPORT]);
    assert_eq!(
        collect_junit("report.zip", &zip)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(xml_documents("report.gz", b"ignored").unwrap().is_empty());
}
#[test]
fn candidates_and_shard_summaries_match_reference() {
    assert_eq!(
        candidate_reports(&[
            json!({"name":"image.tar","size":10}),
            json!({"name":"test.xml","size":10}),
            json!({"name":"too-big.zip","size":MAX_REPORT_BYTES+1}),
            json!({"name":"x.XML","size":10})
        ]),
        vec![json!({"name":"test.xml","size":10})]
    );
    assert_eq!(summarize(&[json!({"shard":1,"cases":[{"suite":"a","name":"x","status":"passed","timeMs":5}]}),json!({"shard":2,"cases":[{"suite":"b","name":"y","status":"failed","message":"boom","timeMs":7},{"suite":"b","name":"z","status":"skipped"}]})]).unwrap(),json!({"total":3,"passed":1,"failed":1,"skipped":1,"timeMs":12,"failures":[{"suite":"b","name":"y","message":"boom","shard":2}]}));
}
#[test]
fn malformed_and_expansion_prone_inputs_are_rejected() {
    assert!(parse_junit("<testsuite><testcase></testsuite>").is_err());
    assert!(parse_junit("<!DOCTYPE testsuite [<!ENTITY x 'boom'>]><testsuite/>").is_err());
    assert!(xml_documents("report.zip", b"not a zip").is_err());
    assert!(xml_documents("report.xml", &vec![b'a'; MAX_REPORT_BYTES as usize + 1]).is_err());
}
