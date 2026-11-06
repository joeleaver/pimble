//! What a plugin tells agents about its viewers (docs/AGENT_GUIDES_CONTRACT.md):
//! the guide a plugin ships as a WebAssembly custom section, read without
//! running the plugin, and the check its settings schema makes.
//!
//! The schema is a small subset of JSON Schema: `type` (one name or several),
//! `properties`, `required`, `additionalProperties` (false, or a schema),
//! `items`, `enum`, `minimum`, `maximum`, `minItems`, `anyOf`, `description`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The custom section a plugin's guide is in.
pub const SECTION: &str = "pimble-guide";

/// The guide format this host reads.
pub const FORMAT: u32 = 1;

/// A plugin's guide: one entry per viewer it provides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub v: u32,
    pub viewers: Vec<ViewerGuide>,
    /// What the plugin's code can be asked to do for an agent (section 8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolGuide>,
}

/// The tool the MCP runs before it writes a viewer's settings:
/// `{ viewer, metanode, settings }`.
pub const CHECK_SETTINGS: &str = "check_settings";
/// The tool the MCP runs before it writes a field a viewer reads:
/// `{ viewer, metanode, node, field, value }`.
pub const CHECK_FIELD: &str = "check_field";

/// One tool a plugin offers agents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolGuide {
    pub name: String,
    pub about: String,
    /// Its arguments, as a schema [`check`] reads.
    pub params: Value,
}

/// What the host asks a plugin's `pimble_tool` export to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRequest {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

/// What a tool answered: whether all was well, and what it said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolAnswer {
    pub ok: bool,
    pub said: String,
}

impl ToolAnswer {
    pub fn ok(said: impl Into<String>) -> Self {
        ToolAnswer { ok: true, said: said.into() }
    }

    pub fn no(said: impl Into<String>) -> Self {
        ToolAnswer { ok: false, said: said.into() }
    }
}

/// What an agent needs to use one viewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewerGuide {
    /// The id a metanode names: `timeline`.
    pub viewer: String,
    /// What it shows and how it reads the metanode's source.
    pub about: String,
    /// The source it wants, as `set_metanode` takes it ("children, two levels").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The node fields it reads on the nodes it shows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldGuide>,
    /// Its settings, kept in the metanode's `data` under the viewer's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SettingsGuide>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldGuide {
    pub name: String,
    pub about: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsGuide {
    pub about: String,
    pub schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
}

impl Guide {
    pub fn parse(bytes: &[u8]) -> Result<Guide, String> {
        let guide: Guide = serde_json::from_slice(bytes).map_err(|e| format!("the guide does not read: {e}"))?;
        if guide.v != FORMAT {
            return Err(format!("the guide is in format {}, and this Pimble reads {FORMAT}", guide.v));
        }
        for viewer in &guide.viewers {
            if let Some(settings) = &viewer.settings {
                if let Some(example) = &settings.example {
                    check(&settings.schema, example).map_err(|e| format!("the {} viewer's example does not fit its schema: {e}", viewer.viewer))?;
                }
            }
        }
        Ok(guide)
    }

    pub fn viewer(&self, id: &str) -> Option<&ViewerGuide> {
        self.viewers.iter().find(|v| v.viewer == id)
    }

    pub fn tool(&self, name: &str) -> Option<&ToolGuide> {
        self.tools.iter().find(|t| t.name == name)
    }
}

/// The guide a module carries, if it carries one: `None` when there is no
/// `pimble-guide` section (or the bytes are not a module), `Some(Err)` when
/// there is one that does not read.
pub fn read(module: &[u8]) -> Option<Result<Guide, String>> {
    custom_section(module, SECTION).map(Guide::parse)
}

/// The payload of the first custom section named `name` in a WebAssembly
/// module.
pub fn custom_section<'a>(module: &'a [u8], name: &str) -> Option<&'a [u8]> {
    if module.len() < 8 || &module[..4] != b"\0asm" {
        return None;
    }
    let mut at = 8;
    while at < module.len() {
        let id = module[at];
        at += 1;
        let (size, read) = leb_u32(&module[at..])?;
        at += read;
        let end = at.checked_add(size as usize).filter(|e| *e <= module.len())?;
        if id == 0 {
            let body = &module[at..end];
            let (len, read) = leb_u32(body)?;
            let name_end = read.checked_add(len as usize).filter(|e| *e <= body.len())?;
            if &body[read..name_end] == name.as_bytes() {
                return Some(&body[name_end..]);
            }
        }
        at = end;
    }
    None
}

fn leb_u32(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    for (i, byte) in bytes.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Does `value` fit `schema`? The refusal names the place ("year[3].days")
/// and what was wanted there.
pub fn check(schema: &Value, value: &Value) -> Result<(), String> {
    check_at(schema, value, "")
}

fn place(path: &str) -> String {
    if path.is_empty() { "the settings object".to_string() } else { path.to_string() }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(n) if n.is_i64() || n.is_u64() => "a whole number",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn fits_type(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|f| f.fract() == 0.0),
        "number" => value.is_number(),
        _ => false,
    }
}

fn wanted(name: &str) -> &'static str {
    match name {
        "object" => "an object",
        "array" => "a list",
        "string" => "text",
        "boolean" => "true or false",
        "null" => "null",
        "integer" => "a whole number",
        "number" => "a number",
        _ => "something else",
    }
}

fn check_at(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(schema) = schema.as_object() else { return Ok(()) };
    if let Some(any) = schema.get("anyOf").and_then(Value::as_array) {
        let mut first_err = None;
        for option in any {
            match check_at(option, value, path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        let shapes: Vec<String> = any.iter().filter_map(|o| o.get("description").and_then(Value::as_str).map(str::to_string)).collect();
        return Err(if shapes.is_empty() {
            first_err.unwrap_or_else(|| format!("{} fits none of the shapes it may take", place(path)))
        } else {
            format!("{} must be one of: {}", place(path), shapes.join("; "))
        });
    }
    match schema.get("type") {
        Some(Value::String(t)) if !fits_type(t, value) => {
            return Err(format!("{} must be {}, not {}", place(path), wanted(t), type_name(value)));
        }
        Some(Value::Array(ts)) if !ts.iter().filter_map(Value::as_str).any(|t| fits_type(t, value)) => {
            let names: Vec<&str> = ts.iter().filter_map(Value::as_str).map(wanted).collect();
            return Err(format!("{} must be {}, not {}", place(path), names.join(" or "), type_name(value)));
        }
        _ => {}
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !options.contains(value) {
            let names: Vec<String> = options.iter().map(Value::to_string).collect();
            return Err(format!("{} must be one of {}", place(path), names.join(", ")));
        }
    }
    if let Some(n) = value.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
            if n < min {
                return Err(format!("{} must be at least {min}", place(path)));
            }
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
            if n > max {
                return Err(format!("{} must be at most {max}", place(path)));
            }
        }
    }
    if let Some(items) = value.as_array() {
        if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                return Err(format!("{} must have at least {min} entries", place(path)));
            }
        }
        if let Some(item) = schema.get("items") {
            for (i, v) in items.iter().enumerate() {
                check_at(item, v, &format!("{}[{i}]", if path.is_empty() { "the list" } else { path }))?;
            }
        }
    }
    if let Some(fields) = value.as_object() {
        let props = schema.get("properties").and_then(Value::as_object);
        for key in schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if !fields.contains_key(key) {
                return Err(format!("{} needs {key}", place(path)));
            }
        }
        for (key, v) in fields {
            let at = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
            match props.and_then(|p| p.get(key)) {
                Some(sub) => check_at(sub, v, &at)?,
                None => match schema.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        let known: Vec<&String> = props.map(|p| p.keys().collect()).unwrap_or_default();
                        let known: Vec<&str> = known.iter().map(|k| k.as_str()).collect();
                        return Err(format!("{} has no {key}; it takes {}", place(path), known.join(", ")));
                    }
                    Some(extra @ Value::Object(_)) => check_at(extra, v, &at)?,
                    _ => {}
                },
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "required": ["name", "year"],
            "additionalProperties": false,
            "properties": {
                "name": { "type": "string" },
                "year": {
                    "type": "array",
                    "minItems": 1,
                    "items": { "anyOf": [
                        { "description": "a month: {\"month\", \"days\"}", "type": "object", "required": ["month", "days"], "properties": { "month": { "type": "string" }, "days": { "type": "integer", "minimum": 1 } } },
                        { "description": "a day of no month: {\"day\"}", "type": "object", "required": ["day"], "properties": { "day": { "type": "string" } } }
                    ] }
                },
                "counts": { "enum": ["up", "down"] }
            }
        })
    }

    #[test]
    fn what_fits_and_what_does_not_says_where() {
        let s = schema();
        assert!(check(&s, &json!({ "name": "Shire", "year": [{ "month": "Afteryule", "days": 30 }, { "day": "Yule" }], "counts": "up" })).is_ok());
        assert_eq!(check(&s, &json!({ "year": [] })).unwrap_err(), "the settings object needs name");
        assert_eq!(check(&s, &json!({ "name": "S", "year": [] })).unwrap_err(), "year must have at least 1 entries");
        let e = check(&s, &json!({ "name": "S", "year": [{ "month": "A", "days": "thirty" }] })).unwrap_err();
        assert!(e.starts_with("year[0] must be one of: a month"), "{e}");
        assert_eq!(check(&s, &json!({ "name": 3, "year": [{ "day": "Y" }] })).unwrap_err(), "name must be text, not a whole number");
        assert_eq!(check(&s, &json!({ "name": "S", "year": [{ "day": "Y" }], "counts": "sideways" })).unwrap_err(), "counts must be one of \"up\", \"down\"");
        let e = check(&s, &json!({ "name": "S", "year": [{ "day": "Y" }], "colour": "red" })).unwrap_err();
        assert!(e.starts_with("the settings object has no colour; it takes"), "{e}");
    }

    #[test]
    fn a_guide_reads_and_its_example_must_fit() {
        let good = json!({ "v": 1, "viewers": [{ "viewer": "cal", "about": "A calendar.", "settings": { "about": "Its definition.", "schema": schema(), "example": { "name": "S", "year": [{ "day": "Y" }] } } }] });
        let guide = Guide::parse(good.to_string().as_bytes()).unwrap();
        assert_eq!(guide.viewer("cal").unwrap().about, "A calendar.");
        let bad = json!({ "v": 1, "viewers": [{ "viewer": "cal", "about": "", "settings": { "about": "", "schema": schema(), "example": { "name": "S" } } }] });
        assert!(Guide::parse(bad.to_string().as_bytes()).unwrap_err().contains("example does not fit"));
        assert!(Guide::parse(br#"{"v": 2, "viewers": []}"#).unwrap_err().contains("format 2"));
    }

    /// A module of a type section and two custom sections, as wasm-ld lays
    /// them out.
    fn module(sections: &[(&str, &[u8])]) -> Vec<u8> {
        let mut m = b"\0asm\x01\0\0\0".to_vec();
        m.extend([1, 1, 0]); // an empty type section
        for (name, payload) in sections {
            let mut body = vec![name.len() as u8];
            body.extend(name.as_bytes());
            body.extend(*payload);
            m.push(0);
            let mut size = body.len() as u32;
            loop {
                let byte = (size & 0x7f) as u8;
                size >>= 7;
                if size == 0 {
                    m.push(byte);
                    break;
                }
                m.push(byte | 0x80);
            }
            m.extend(body);
        }
        m
    }

    #[test]
    fn the_guide_is_read_from_its_section_and_nothing_else_is() {
        let guide = br#"{"v":1,"viewers":[{"viewer":"kanban","about":"Columns of cards."}]}"#;
        let long = vec![b'x'; 300];
        let m = module(&[("producers", &long), (SECTION, guide)]);
        assert_eq!(read(&m).unwrap().unwrap().viewers[0].viewer, "kanban");
        assert!(read(&module(&[("producers", b"x")])).is_none());
        assert!(read(b"not a module").is_none());
        let mut cut = m.clone();
        cut.truncate(m.len() - 10);
        assert!(read(&cut).is_none(), "a section running past the end is not read");
    }
}
