//! Device observation helpers.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::app::App;
use crate::error::{CliError, CliResult};
use crate::process::FailureMode;

const UNIQUE_ATTRIBUTE_LIMIT: usize = 24;

#[derive(Clone, Copy)]
pub(crate) enum AccessibilityDetail {
    Compressed,
    Full,
}

#[derive(Clone, Copy)]
pub(crate) struct StateOptions<'a> {
    pub(crate) include_accessibility: bool,
    pub(crate) screenshot_out: Option<&'a Path>,
    pub(crate) accessibility_detail: AccessibilityDetail,
}

#[derive(Clone, Copy)]
enum AccessibilityXmlOutput {
    IncludeWhenNoFile,
    Omit,
}

pub(crate) fn accessibility(
    app: &App,
    out: Option<&Path>,
    detail: AccessibilityDetail,
) -> CliResult<Value> {
    capture_accessibility(app, out, detail, AccessibilityXmlOutput::IncludeWhenNoFile)
}

pub(crate) fn screenshot(app: &App, out: &Path) -> CliResult<Value> {
    capture_screenshot(app, out)
}

pub(crate) fn state(app: &App, options: StateOptions<'_>) -> CliResult<Value> {
    let status = app.broadcast("STATUS", Vec::new())?;
    let layout = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
    let accessibility = if options.include_accessibility {
        capture_accessibility(
            app,
            None,
            options.accessibility_detail,
            AccessibilityXmlOutput::Omit,
        )?
    } else {
        Value::Null
    };
    let screenshot = options
        .screenshot_out
        .map_or(Ok(Value::Null), |path| capture_screenshot(app, path))?;
    Ok(state_json(
        app,
        &status,
        &layout,
        &accessibility,
        &screenshot,
    ))
}

pub(crate) fn all(
    app: &App,
    out_dir: &Path,
    accessibility_detail: AccessibilityDetail,
) -> CliResult<Value> {
    fs::create_dir_all(out_dir)?;

    let status_path = out_dir.join("status.json");
    let layout_path = out_dir.join("layout.json");
    let accessibility_path = out_dir.join("accessibility.xml");
    let screenshot_path = out_dir.join("screenshot.png");
    let state_path = out_dir.join("state.json");
    let index_path = out_dir.join("index.json");

    let status = app.broadcast("STATUS", Vec::new())?;
    write_json_file(&status_path, &status)?;
    let layout = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
    write_json_file(&layout_path, &layout)?;
    let accessibility = capture_accessibility(
        app,
        Some(&accessibility_path),
        accessibility_detail,
        AccessibilityXmlOutput::Omit,
    )?;
    let screenshot = capture_screenshot(app, &screenshot_path)?;
    let state = state_json(app, &status, &layout, &accessibility, &screenshot);
    write_json_file(&state_path, &state)?;

    let index = json!({
        "ok": true,
        "schema": "input_dynamics_observation_bundle.v1",
        "package_name": app.package(),
        "captured_wall_ms": epoch_millis()?,
        "output_dir": path_string(out_dir)?,
        "artifacts": {
            "status_json": path_string(&status_path)?,
            "layout_json": path_string(&layout_path)?,
            "accessibility_xml": path_string(&accessibility_path)?,
            "screenshot_png": path_string(&screenshot_path)?,
            "state_json": path_string(&state_path)?,
            "index_json": path_string(&index_path)?,
        },
        "state": state,
    });
    write_json_file(&index_path, &index)?;
    Ok(index)
}

fn state_json(
    app: &App,
    status: &Value,
    layout: &Value,
    accessibility: &Value,
    screenshot: &Value,
) -> Value {
    json!({
        "ok": status.get("ok").and_then(Value::as_bool).unwrap_or(false)
            && layout.get("ok").and_then(Value::as_bool).unwrap_or(false)
            && optional_observation_ok(accessibility)
            && optional_observation_ok(screenshot),
        "schema": "input_dynamics_observation_state.v1",
        "package_name": app.package(),
        "captured_wall_ms": epoch_millis().map_or(Value::Null, |millis| json!(millis)),
        "status": status,
        "layout": layout,
        "accessibility": accessibility,
        "screenshot": screenshot,
    })
}

fn optional_observation_ok(value: &Value) -> bool {
    value.is_null() || value.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn capture_accessibility(
    app: &App,
    out: Option<&Path>,
    detail: AccessibilityDetail,
    xml_output: AccessibilityXmlOutput,
) -> CliResult<Value> {
    let remote = remote_temp_path("accessibility", "xml")?;
    let compressed = matches!(detail, AccessibilityDetail::Compressed);
    let mut dump_args = vec![String::from("uiautomator"), String::from("dump")];
    if compressed {
        dump_args.push(String::from("--compressed"));
    }
    dump_args.push(remote.clone());

    let dump = match app.adb_shell(dump_args, FailureMode::RequireSuccess) {
        Ok(output) => output,
        Err(error) => {
            let cleanup = cleanup_remote(app, &remote);
            return Err(CliError::new(format!(
                "failed to dump accessibility hierarchy: {error}; cleanup: {cleanup}"
            )));
        }
    };
    let cat = match app.adb_shell(
        vec![String::from("cat"), remote.clone()],
        FailureMode::RequireSuccess,
    ) {
        Ok(output) => output,
        Err(error) => {
            let cleanup = cleanup_remote(app, &remote);
            return Err(CliError::new(format!(
                "failed to read accessibility hierarchy: {error}; cleanup: {cleanup}"
            )));
        }
    };
    let cleanup = cleanup_remote(app, &remote);
    let xml = cat.stdout();
    let summary = accessibility_summary(xml);

    let out_path = out.map(path_string).transpose()?;
    if let Some(path) = out {
        ensure_parent_dir(path)?;
        fs::write(path, xml)?;
    }

    let mut result = Map::new();
    result.insert(String::from("ok"), json!(true));
    result.insert(
        String::from("schema"),
        json!("input_dynamics_accessibility_observation.v1"),
    );
    result.insert(String::from("package_name"), json!(app.package()));
    result.insert(String::from("captured_wall_ms"), json!(epoch_millis()?));
    result.insert(String::from("compressed"), json!(compressed));
    result.insert(String::from("remote_path"), json!(remote));
    result.insert(
        String::from("output_path"),
        out_path.map_or(Value::Null, |path| json!(path)),
    );
    result.insert(String::from("summary"), summary);
    result.insert(String::from("dump"), dump.json());
    result.insert(String::from("cleanup"), cleanup);
    if out.is_none() && matches!(xml_output, AccessibilityXmlOutput::IncludeWhenNoFile) {
        result.insert(String::from("xml"), json!(xml));
    }
    Ok(Value::Object(result))
}

fn capture_screenshot(app: &App, out: &Path) -> CliResult<Value> {
    ensure_parent_dir(out)?;
    let remote = remote_temp_path("screenshot", "png")?;
    let screencap = match app.adb_shell(
        vec![
            String::from("screencap"),
            String::from("-p"),
            remote.clone(),
        ],
        FailureMode::RequireSuccess,
    ) {
        Ok(output) => output,
        Err(error) => {
            let cleanup = cleanup_remote(app, &remote);
            return Err(CliError::new(format!(
                "failed to capture screenshot: {error}; cleanup: {cleanup}"
            )));
        }
    };
    let pull = match app.adb(
        &[String::from("pull"), remote.clone(), path_string(out)?],
        FailureMode::RequireSuccess,
    ) {
        Ok(output) => output,
        Err(error) => {
            let cleanup = cleanup_remote(app, &remote);
            return Err(CliError::new(format!(
                "failed to pull screenshot: {error}; cleanup: {cleanup}"
            )));
        }
    };
    let cleanup = cleanup_remote(app, &remote);
    let metadata = fs::metadata(out)?;
    Ok(json!({
        "ok": true,
        "schema": "input_dynamics_screenshot_observation.v1",
        "package_name": app.package(),
        "captured_wall_ms": epoch_millis()?,
        "remote_path": remote,
        "output_path": path_string(out)?,
        "byte_count": metadata.len(),
        "screencap": screencap.json(),
        "pull": pull.json(),
        "cleanup": cleanup,
    }))
}

fn accessibility_summary(xml: &str) -> Value {
    json!({
        "xml_byte_count": xml.len(),
        "node_count": xml.matches("<node").count(),
        "focused_node_count": xml.matches("focused=\"true\"").count(),
        "selected_node_count": xml.matches("selected=\"true\"").count(),
        "clickable_node_count": xml.matches("clickable=\"true\"").count(),
        "scrollable_node_count": xml.matches("scrollable=\"true\"").count(),
        "password_node_count": xml.matches("password=\"true\"").count(),
        "non_empty_text_attribute_count": non_empty_attribute_count(xml, "text"),
        "non_empty_content_desc_attribute_count": non_empty_attribute_count(xml, "content-desc"),
        "unique_packages": unique_attribute_values(xml, "package"),
        "class_sample": unique_attribute_values(xml, "class"),
    })
}

fn non_empty_attribute_count(xml: &str, attribute: &str) -> usize {
    let marker = format!("{attribute}=\"");
    xml.split(marker.as_str())
        .skip(1)
        .filter_map(|segment| segment.split('"').next())
        .filter(|value| !value.is_empty())
        .count()
}

fn unique_attribute_values(xml: &str, attribute: &str) -> Vec<String> {
    let marker = format!("{attribute}=\"");
    let mut values = BTreeSet::new();
    for segment in xml.split(marker.as_str()).skip(1) {
        if let Some(value) = segment.split('"').next().filter(|value| !value.is_empty()) {
            values.insert(String::from(value));
        }
    }
    values.into_iter().take(UNIQUE_ATTRIBUTE_LIMIT).collect()
}

fn cleanup_remote(app: &App, remote: &str) -> Value {
    match app.adb_shell(
        vec![String::from("rm"), String::from("-f"), String::from(remote)],
        FailureMode::AllowFailure,
    ) {
        Ok(output) => output.json(),
        Err(error) => json!({
            "status_code": Value::Null,
            "stdout": "",
            "stderr": error.to_string(),
        }),
    }
}

fn remote_temp_path(kind: &str, extension: &str) -> CliResult<String> {
    Ok(format!(
        "/data/local/tmp/input-dynamics-observe-{kind}-{}-{}.{}",
        std::process::id(),
        epoch_millis()?,
        extension
    ))
}

fn ensure_parent_dir(path: &Path) -> CliResult<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn write_json_file(path: &Path, value: &Value) -> CliResult<()> {
    ensure_parent_dir(path)?;
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, format!("{text}\n"))?;
    Ok(())
}

fn path_string(path: &Path) -> CliResult<String> {
    path.to_str()
        .map(String::from)
        .ok_or_else(|| CliError::new(format!("path is not valid UTF-8: {}", path.display())))
}

fn epoch_millis() -> CliResult<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::new(format!("system clock is before Unix epoch: {error}")))?
        .as_millis();
    u64::try_from(millis).map_err(|error| CliError::new(format!("millis overflow: {error}")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::observe::{accessibility_summary, unique_attribute_values};

    #[test]
    fn accessibility_summary_counts_without_exposing_text_values() {
        let xml = r#"<hierarchy>
            <node text="secret" content-desc="" package="pkg.one" class="android.widget.EditText" focused="true" clickable="true" password="false" />
            <node text="" content-desc="button" package="pkg.two" class="android.widget.Button" focused="false" clickable="true" password="true" />
        </hierarchy>"#;

        let summary = accessibility_summary(xml);

        assert_eq!(summary.get("node_count"), Some(&json!(2_usize)));
        assert_eq!(summary.get("focused_node_count"), Some(&json!(1_usize)));
        assert_eq!(summary.get("clickable_node_count"), Some(&json!(2_usize)));
        assert_eq!(summary.get("password_node_count"), Some(&json!(1_usize)));
        assert_eq!(
            summary.get("non_empty_text_attribute_count"),
            Some(&json!(1_usize))
        );
        assert_eq!(
            summary.get("non_empty_content_desc_attribute_count"),
            Some(&json!(1_usize))
        );
        assert_eq!(
            summary.get("unique_packages"),
            Some(&json!(["pkg.one", "pkg.two"]))
        );
    }

    #[test]
    fn unique_attribute_values_are_sorted_and_limited() {
        let xml = r#"<node package="b" /><node package="a" /><node package="b" />"#;

        let values = unique_attribute_values(xml, "package");

        assert_eq!(values, vec![String::from("a"), String::from("b")]);
    }
}
