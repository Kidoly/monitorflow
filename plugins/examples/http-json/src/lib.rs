//! A check that fetches a web address (ADR-0103). Reports, under the
//! check's name: `up` (1 for a 2xx answer, else 0), `status_code`,
//! `seconds` and, with the JSON field setting, `value`.

use monitorflow_plugin_sdk::serde_json::Value;
use monitorflow_plugin_sdk::{check, CheckInput, Request, Sample};

fn run(input: CheckInput) -> Result<Vec<Sample>, String> {
    let url = input.setting("url")?;
    let response = match Request::get(url)
        .header("Accept", "application/json")
        .send()
    {
        Ok(response) => response,
        // Down is a measurement, not a failure of the check.
        Err(error) => {
            monitorflow_plugin_sdk::log(monitorflow_plugin_sdk::Level::Info, &error);
            return Ok(vec![Sample::new("up", 0.0)]);
        }
    };
    let mut samples = vec![
        Sample::new("up", if response.is_success() { 1.0 } else { 0.0 }),
        Sample::new("status_code", f64::from(response.status)),
        Sample::new("seconds", response.elapsed_ms as f64 / 1000.0),
    ];
    if let Some(field) = input.settings.get("field").filter(|f| !f.is_empty()) {
        let value = number_at(&response.json()?, field)
            .ok_or_else(|| format!("the answer has no number at {field}"))?;
        samples.push(Sample::new("value", value));
    }
    Ok(samples)
}

/// The number at a dotted path; array items by index, like `items.0.size`.
/// Strings holding a number count.
pub fn number_at(value: &Value, path: &str) -> Option<f64> {
    let found = path.split('.').try_fold(value, |v, key| match v {
        Value::Array(items) => items.get(key.parse::<usize>().ok()?),
        _ => v.get(key),
    })?;
    match found {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
    .filter(|n| n.is_finite())
}

check!(run);

#[cfg(test)]
mod tests {
    use monitorflow_plugin_sdk::serde_json::json;

    use super::*;

    #[test]
    fn numbers_are_found_by_path() {
        let value =
            json!({"data": {"queue": {"length": 4}, "items": [{"size": "2.5"}], "ok": true}});
        assert_eq!(number_at(&value, "data.queue.length"), Some(4.0));
        assert_eq!(number_at(&value, "data.items.0.size"), Some(2.5));
        assert_eq!(number_at(&value, "data.ok"), Some(1.0));
        assert_eq!(number_at(&value, "data.queue"), None);
        assert_eq!(number_at(&value, "data.missing"), None);
    }
}
