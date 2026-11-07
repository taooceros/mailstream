//! JSON bytes as the Swift engine's `JSONEncoder` writes them, so files both engines write
//! (settings.json, accounts.json, the Keychain item) come out the same whichever wrote last:
//! keys sorted (serde_json's map is ordered), `"key" : value` with two-space indents when pretty,
//! empty containers as `[\n\n]`, integral numbers without a fraction and, unless asked not to,
//! slashes escaped as `\/`.
use std::io::{self, Write};

use serde::Serialize;
use serde_json::ser::Formatter;

/// `value` encoded; `pretty` indents, `escape_slashes` writes `/` as `\/` (Swift's default).
pub fn encode<T: Serialize + ?Sized>(value: &T, pretty: bool, escape_slashes: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    let formatter = Apple { pretty, escape_slashes, indent: 0, has_value: false };
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    value.serialize(&mut serializer).expect("JSON values encode into memory");
    out
}

struct Apple {
    pretty: bool,
    escape_slashes: bool,
    indent: usize,
    has_value: bool,
}

impl Apple {
    fn newline<W: ?Sized + Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b"\n")?;
        for _ in 0..self.indent {
            writer.write_all(b"  ")?;
        }
        Ok(())
    }

    fn open<W: ?Sized + Write>(&mut self, writer: &mut W, bracket: &[u8]) -> io::Result<()> {
        self.indent += 1;
        self.has_value = false;
        writer.write_all(bracket)
    }

    fn close<W: ?Sized + Write>(&mut self, writer: &mut W, bracket: &[u8]) -> io::Result<()> {
        self.indent -= 1;
        if self.pretty {
            if !self.has_value {
                writer.write_all(b"\n")?;
            }
            self.newline(writer)?;
        }
        writer.write_all(bracket)
    }

    fn item<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if !first {
            writer.write_all(b",")?;
        }
        if self.pretty { self.newline(writer) } else { Ok(()) }
    }
}

impl Formatter for Apple {
    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.open(writer, b"[")
    }

    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.close(writer, b"]")
    }

    fn begin_array_value<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        self.item(writer, first)
    }

    fn end_array_value<W: ?Sized + Write>(&mut self, _writer: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }

    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.open(writer, b"{")
    }

    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.close(writer, b"}")
    }

    fn begin_object_key<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        self.item(writer, first)
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(if self.pretty { b" : " } else { b":" })
    }

    fn end_object_value<W: ?Sized + Write>(&mut self, _writer: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }

    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        if value.fract() == 0.0 && value.abs() < 1e15 {
            write!(writer, "{}", value as i64)
        } else {
            writer.write_all(ryu_like(value).as_bytes())
        }
    }

    fn write_string_fragment<W: ?Sized + Write>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()> {
        if !self.escape_slashes {
            return writer.write_all(fragment.as_bytes());
        }
        for (index, piece) in fragment.split('/').enumerate() {
            if index > 0 {
                writer.write_all(b"\\/")?;
            }
            writer.write_all(piece.as_bytes())?;
        }
        Ok(())
    }
}

/// A non-integral double as serde_json writes it.
fn ryu_like(value: f64) -> String {
    serde_json::Number::from_f64(value).map_or_else(|| "null".into(), |number| number.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::encode;

    #[test]
    fn writes_like_swift_json_encoder() {
        let value = json!({
            "b": [], "a": ["x/y", "é\u{1}\"\\\t\n"], "c": 1.5, "e": {"z": 1, "y": {}}, "f": null, "h": 100.0,
        });
        let text = String::from_utf8(encode(&value, true, false)).unwrap();
        let expected = "{\n  \"a\" : [\n    \"x/y\",\n    \"é\\u0001\\\"\\\\\\t\\n\"\n  ],\n  \"b\" : [\n\n  ],\n  \"c\" : 1.5,\n  \"e\" : {\n    \"y\" : {\n\n    },\n    \"z\" : 1\n  },\n  \"f\" : null,\n  \"h\" : 100\n}";
        assert_eq!(text, expected);
        assert_eq!(String::from_utf8(encode(&json!({}), true, false)).unwrap(), "{\n\n}");
        assert_eq!(String::from_utf8(encode(&json!([]), true, true)).unwrap(), "[\n\n]");
    }

    #[test]
    fn escapes_slashes_compactly_when_asked() {
        let value = json!({"refreshTokens": {"a@b": "1//x"}, "clients": {"a@b": {"id": "i"}}, "lastClients": {}});
        assert_eq!(
            String::from_utf8(encode(&value, false, true)).unwrap(),
            r#"{"clients":{"a@b":{"id":"i"}},"lastClients":{},"refreshTokens":{"a@b":"1\/\/x"}}"#
        );
    }
}
