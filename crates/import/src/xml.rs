//! XML read into a JSON value, so a reader handles XML, JSON and YAML
//! exports with one set of serde types.
//!
//! An element with text only becomes a string; one with children becomes
//! an object; attributes become keys too. A container whose children all
//! have one name that is its own in the singular (`<hosts><host/></hosts>`,
//! `<dependencies><dependency/></dependencies>`) becomes an array, and so
//! does a repeated child name. An empty element becomes an empty string,
//! which readers take as absent. Text beside attributes or children goes
//! under `#text`.

use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{Map, Value};

/// Deepest nesting read, against a hostile file.
const MAX_DEPTH: usize = 64;

/// An element being read: its name, its children in order, its text.
type Open = (String, Vec<(String, Value)>, String);

/// The root element's content, keyed by the root's name:
/// `{"zabbix_export": {...}}`.
pub fn to_json(text: &str) -> Result<Value, String> {
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<Open> = vec![(String::new(), Vec::new(), String::new())];
    loop {
        match reader.read_event() {
            Err(e) => {
                return Err(format!(
                    "not valid XML at byte {}: {e}",
                    reader.buffer_position()
                ))
            }
            Ok(Event::Start(start)) => {
                if stack.len() > MAX_DEPTH {
                    return Err("nested too deeply".into());
                }
                let name = name_of(start.name().as_ref());
                stack.push((name, attributes(&start)?, String::new()));
            }
            Ok(Event::Empty(start)) => {
                let name = name_of(start.name().as_ref());
                let value = build(&name, attributes(&start)?, String::new());
                if let Some(top) = stack.last_mut() {
                    top.1.push((name, value));
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(top) = stack.last_mut() {
                    top.2.push_str(&text.xml10_content());
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                let name: &str = reference.as_ref();
                let text = match name {
                    "lt" => "<".to_owned(),
                    "gt" => ">".to_owned(),
                    "amp" => "&".to_owned(),
                    "apos" => "'".to_owned(),
                    "quot" => "\"".to_owned(),
                    other => {
                        let code = other
                            .strip_prefix("#x")
                            .and_then(|h| u32::from_str_radix(h, 16).ok())
                            .or_else(|| other.strip_prefix('#').and_then(|d| d.parse().ok()));
                        code.and_then(char::from_u32)
                            .map(String::from)
                            .ok_or_else(|| format!("unknown entity &{other};"))?
                    }
                };
                if let Some(top) = stack.last_mut() {
                    top.2.push_str(&text);
                }
            }
            Ok(Event::CData(data)) => {
                if let Some(top) = stack.last_mut() {
                    top.2.push_str(data.as_ref());
                }
            }
            Ok(Event::End(_)) => {
                let (name, children, text) = stack.pop().ok_or("unbalanced XML")?;
                let value = build(&name, children, text);
                match stack.last_mut() {
                    Some(top) => top.1.push((name, value)),
                    None => return Err("unbalanced XML".into()),
                }
            }
            Ok(Event::DocType(_)) => return Err("document types are not read".into()),
            Ok(Event::Eof) => break,
            Ok(_) => {}
        }
    }
    let (_, mut root, _) = stack.pop().ok_or("empty document")?;
    if stack.is_empty() && root.len() == 1 {
        let (name, value) = root.remove(0);
        let mut object = Map::new();
        object.insert(name, value);
        Ok(Value::Object(object))
    } else {
        Err("an XML document has exactly one root element".into())
    }
}

fn name_of(name: &str) -> String {
    name.to_owned()
}

/// The attributes of an element, entities resolved.
fn attributes(start: &quick_xml::events::BytesStart) -> Result<Vec<(String, Value)>, String> {
    let mut out = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|e| e.to_string())?;
        let value = quick_xml::escape::unescape(&attribute.value).map_err(|e| e.to_string())?;
        out.push((
            name_of(attribute.key.as_ref()),
            Value::String(value.into_owned()),
        ));
    }
    Ok(out)
}

fn build(name: &str, mut children: Vec<(String, Value)>, text: String) -> Value {
    let text = text.trim();
    if children.is_empty() {
        return Value::String(text.to_owned());
    }
    if !text.is_empty() {
        children.push(("#text".to_owned(), Value::String(text.to_owned())));
    }
    let first = &children[0].0;
    let same = children.iter().all(|(n, _)| n == first);
    if same && is_plural_of(name, first) {
        return Value::Array(children.into_iter().map(|(_, v)| v).collect());
    }
    let mut object = Map::new();
    for (key, value) in children {
        match object.get_mut(&key) {
            Some(Value::Array(items)) if repeated(&key) => items.push(value),
            Some(existing) => {
                let first = existing.take();
                *existing = Value::Array(vec![first, value]);
                REPEATED.with(|r| r.borrow_mut().push(key));
            }
            None => {
                object.insert(key, value);
            }
        }
    }
    REPEATED.with(|r| r.borrow_mut().clear());
    Value::Object(object)
}

thread_local! {
    /// Keys turned into arrays while building one object.
    static REPEATED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn repeated(key: &str) -> bool {
    REPEATED.with(|r| r.borrow().iter().any(|k| k == key))
}

/// `hosts`/`host`, `dependencies`/`dependency`, `statuses`/`status`.
fn is_plural_of(plural: &str, singular: &str) -> bool {
    plural == format!("{singular}s")
        || plural == format!("{singular}es")
        || singular
            .strip_suffix('y')
            .is_some_and(|stem| plural == format!("{stem}ies"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn containers_become_arrays() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<zabbix_export>
  <version>7.0</version>
  <hosts>
    <host>
      <host>web-01</host>
      <groups><group><name>Linux servers</name></group></groups>
      <tags/>
      <dependencies><dependency><name>a</name></dependency></dependencies>
      <description>a &lt;b&gt; &amp; c</description>
    </host>
  </hosts>
</zabbix_export>"#;
        assert_eq!(
            to_json(xml).unwrap(),
            json!({"zabbix_export": {
                "version": "7.0",
                "hosts": [{
                    "host": "web-01",
                    "groups": [{"name": "Linux servers"}],
                    "tags": "",
                    "dependencies": [{"name": "a"}],
                    "description": "a <b> & c",
                }],
            }})
        );
    }

    #[test]
    fn attributes_and_repeated_children() {
        let xml = r#"<root><item id="1">x</item><item id="2"><a>1</a></item></root>"#;
        assert_eq!(
            to_json(xml).unwrap(),
            json!({"root": {"item": [{"id": "1", "#text": "x"}, {"id": "2", "a": "1"}]}})
        );
        let xml = r#"<root><n>1</n><n>2</n><n>3</n></root>"#;
        assert_eq!(
            to_json(xml).unwrap(),
            json!({"root": {"n": ["1", "2", "3"]}})
        );
    }

    #[test]
    fn hostile_documents_are_refused() {
        assert!(to_json("<a><b></a>").is_err());
        assert!(to_json("<!DOCTYPE x [<!ENTITY e \"y\">]><a>&e;</a>").is_err());
        let deep = "<a>".repeat(100) + &"</a>".repeat(100);
        assert!(to_json(&deep).is_err());
        assert!(to_json("").is_err());
    }
}
