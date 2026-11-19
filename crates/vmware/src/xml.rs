//! A small element tree of SOAP answers, by local names: namespaces and
//! prefixes are dropped, which vim25 answers never need to tell apart.

use quick_xml::events::Event;
use quick_xml::Reader;

/// One element: its local name, attributes by local name, text and
/// children.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Element>,
}

/// Deepest nesting read; vim25 answers stay far below it.
const MAX_DEPTH: usize = 64;

fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_owned()
}

fn element(start: &quick_xml::events::BytesStart<'_>) -> Result<Element, String> {
    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|e| e.to_string())?;
        let value = quick_xml::escape::unescape(&attribute.value)
            .map_err(|e| e.to_string())?
            .into_owned();
        attributes.push((local(attribute.key.as_ref()), value));
    }
    Ok(Element {
        name: local(start.name().as_ref()),
        attributes,
        ..Element::default()
    })
}

/// Parses a whole document into its root element.
pub fn parse(text: &str) -> Result<Element, String> {
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<Element> = Vec::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(start) => {
                if stack.len() >= MAX_DEPTH {
                    return Err("the answer is nested too deeply".into());
                }
                stack.push(element(&start)?);
            }
            Event::Empty(start) => {
                let done = element(&start)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => return Ok(done),
                }
            }
            Event::End(_) => {
                let done = stack.pop().ok_or("unbalanced answer")?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => return Ok(done),
                }
            }
            Event::Text(text) => {
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&text.xml10_content());
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(current) = stack.last_mut() {
                    let name: &str = reference.as_ref();
                    let entity = format!("&{name};");
                    let text = quick_xml::escape::unescape(&entity).map_err(|e| e.to_string())?;
                    current.text.push_str(&text);
                }
            }
            Event::CData(data) => {
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(data.as_ref());
                }
            }
            Event::DocType(_) => return Err("document types are not read".into()),
            Event::Eof => return Err("the answer ended early".into()),
            _ => {}
        }
    }
}

impl Element {
    /// The first child named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child named `name`.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The element at `path` of child names.
    pub fn path(&self, path: &[&str]) -> Option<&Element> {
        path.iter().try_fold(self, |e, name| e.child(name))
    }

    /// The trimmed text of the child `name`.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.trim())
    }

    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The first descendant named `name`, depth first.
    pub fn find(&self, name: &str) -> Option<&Element> {
        self.children.iter().find_map(|c| {
            if c.name == name {
                Some(c)
            } else {
                c.find(name)
            }
        })
    }
}

/// Escapes `value` for element text.
pub fn escape(value: &str) -> String {
    quick_xml::escape::escape(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_local_and_text_unescaped() {
        let root = parse(
            r#"<?xml version="1.0"?><s:Envelope xmlns:s="x"><s:Body><a xsi:type="t">x &amp; y<b/></a><c><![CDATA[<z>]]></c></s:Body></s:Envelope>"#,
        )
        .unwrap();
        assert_eq!(root.name, "Envelope");
        let a = root.path(&["Body", "a"]).unwrap();
        assert_eq!(a.text, "x & y");
        assert_eq!(a.attribute("type"), Some("t"));
        assert!(a.child("b").is_some());
        assert_eq!(root.find("c").unwrap().text, "<z>");
        assert!(parse("<a><b></a>").is_err());
        assert!(parse("<a>").is_err());
        assert_eq!(escape("<&>"), "&lt;&amp;&gt;");
    }

    #[test]
    fn deep_nesting_is_refused() {
        let deep = "<a>".repeat(100) + &"</a>".repeat(100);
        assert!(parse(&deep).is_err());
    }
}
