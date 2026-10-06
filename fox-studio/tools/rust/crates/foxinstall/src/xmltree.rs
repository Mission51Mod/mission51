//! A small ordered XML tree with a writer that reproduces .NET XmlSerializer output (SnakeBite's snakebite.xml):
//! `<?xml version="1.0"?>`, LF, two-space indentation, `<Tag attr="..." />` for empty elements, text elements on one
//! line. Attribute order and values are kept exactly as read.
use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Elem {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Elem>,
    /// text content (only for leaf elements such as Description)
    pub text: Option<String>,
}

impl Elem {
    pub fn new(name: &str) -> Elem {
        Elem { name: name.into(), ..Default::default() }
    }
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
    pub fn with(mut self, k: &str, v: impl Into<String>) -> Elem {
        self.attrs.push((k.into(), v.into()));
        self
    }
    pub fn child(&self, n: &str) -> Option<&Elem> {
        self.children.iter().find(|c| c.name == n)
    }
    pub fn child_mut(&mut self, n: &str) -> Option<&mut Elem> {
        self.children.iter_mut().find(|c| c.name == n)
    }
}

pub struct Doc {
    pub decl: String,
    /// line ending of the source (CRLF for SnakeBite's files)
    pub nl: String,
    pub root: Elem,
}

pub fn parse(text: &str) -> Result<Doc, String> {
    let mut r = Reader::from_str(text);
    r.config_mut().trim_text(false);
    let mut stack: Vec<Elem> = vec![];
    let mut root: Option<Elem> = None;
    let mut decl = String::new();
    loop {
        match r.read_event().map_err(|e| format!("xml: {e}"))? {
            Event::Decl(_) => {
                decl = text.split_once("?>").map(|(a, _)| format!("{a}?>")).unwrap_or_default();
            }
            Event::Start(e) => {
                let mut el = Elem::new(e.name().0);
                for a in e.attributes() {
                    let a = a.map_err(|x| x.to_string())?;
                    #[allow(deprecated)]
                    let v = a.unescape_value().map_err(|x| x.to_string())?.to_string();
                    el.attrs.push((a.key.0.to_string(), v));
                }
                stack.push(el);
            }
            Event::Empty(e) => {
                let mut el = Elem::new(e.name().0);
                for a in e.attributes() {
                    let a = a.map_err(|x| x.to_string())?;
                    #[allow(deprecated)]
                    let v = a.unescape_value().map_err(|x| x.to_string())?.to_string();
                    el.attrs.push((a.key.0.to_string(), v));
                }
                match stack.last_mut() {
                    Some(p) => p.children.push(el),
                    None => root = Some(el),
                }
            }
            Event::Text(t) => {
                let raw = t[..].to_string();
                if let Some(p) = stack.last_mut()
                    && (!raw.trim().is_empty() || p.text.is_some())
                {
                    let un = quick_xml::escape::unescape(&raw).map_err(|e| e.to_string())?.to_string();
                    p.text.get_or_insert_with(String::new).push_str(&un);
                }
            }
            Event::GeneralRef(g) => {
                if let Some(p) = stack.last_mut() {
                    let ent = format!("&{};", &g[..]);
                    let un = quick_xml::escape::unescape(&ent).map_err(|e| e.to_string())?.to_string();
                    p.text.get_or_insert_with(String::new).push_str(&un);
                }
            }
            Event::End(_) => {
                let el = stack.pop().ok_or("unbalanced xml")?;
                match stack.last_mut() {
                    Some(p) => p.children.push(el),
                    None => root = Some(el),
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" }.to_string();
    Ok(Doc { decl, nl, root: root.ok_or("empty xml")? })
}

fn esc_attr(s: &str, o: &mut String) {
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\n' => o.push_str("&#xA;"),
            '\r' => o.push_str("&#xD;"),
            '\t' => o.push_str("&#x9;"),
            c => o.push(c),
        }
    }
}

fn esc_text(s: &str, o: &mut String) {
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            c => o.push(c),
        }
    }
}

fn write_el(e: &Elem, depth: usize, o: &mut String, nl: &str) {
    for _ in 0..depth {
        o.push_str("  ");
    }
    o.push('<');
    o.push_str(&e.name);
    for (k, v) in &e.attrs {
        o.push(' ');
        o.push_str(k);
        o.push_str("=\"");
        esc_attr(v, o);
        o.push('"');
    }
    if e.children.is_empty() && e.text.is_none() {
        o.push_str(" />");
        o.push_str(nl);
        return;
    }
    o.push('>');
    if let Some(t) = &e.text {
        // text keeps its own line breaks (XmlSerializer writes them as they are)
        esc_text(t, o);
        o.push_str("</");
        o.push_str(&e.name);
        o.push('>');
        o.push_str(nl);
        return;
    }
    o.push_str(nl);
    for c in &e.children {
        write_el(c, depth + 1, o, nl);
    }
    for _ in 0..depth {
        o.push_str("  ");
    }
    o.push_str("</");
    o.push_str(&e.name);
    o.push('>');
    o.push_str(nl);
}

pub fn write(d: &Doc) -> String {
    let mut o = String::new();
    o.push_str(&d.decl);
    o.push_str(&d.nl);
    write_el(&d.root, 0, &mut o, &d.nl);
    // XmlSerializer writes no line break after the root's closing tag
    if o.ends_with(d.nl.as_str()) {
        o.truncate(o.len() - d.nl.len());
    }
    o
}

/// one element (and its subtree) as text, LF line breaks, no indentation offset
pub fn write_elem(e: &Elem) -> String {
    let mut o = String::new();
    write_el(e, 0, &mut o, "\n");
    o
}

/// parse one element written by write_elem
pub fn parse_elem(text: &str) -> Result<Elem, String> {
    Ok(parse(text)?.root)
}

/// re-indent an element subtree for a document at `depth`
pub fn write_elem_at(e: &Elem, depth: usize, nl: &str) -> String {
    let mut o = String::new();
    write_el(e, depth, &mut o, nl);
    o
}
