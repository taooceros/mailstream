//! JSON bytes as the Swift engine's `JSONEncoder` writes them, so files both engines write
//! (settings.json, accounts.json, the Keychain item) come out the same whichever wrote last:
//! map keys sorted (objects, `BTreeMap`/`HashMap` fields; structs keep their fields' order),
//! `"key" : value` with two-space indents when pretty, empty containers as `[\n\n]`, integral
//! numbers without a fraction and, unless asked not to, slashes escaped as `\/`. serde_json keeps
//! objects in insertion order (`preserve_order`, MS-303), so the sorting is done here.
use std::borrow::Cow;
use std::io::{self, Write};

use serde::Serialize;
use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple, SerializeTupleStruct,
    SerializeTupleVariant, Serializer,
};
use serde_json::{Value, ser::Formatter};

/// `value` encoded; `pretty` indents, `escape_slashes` writes `/` as `\/` (Swift's default).
pub fn encode<T: Serialize + ?Sized>(value: &T, pretty: bool, escape_slashes: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    let formatter = Apple { pretty, escape_slashes, indent: 0, has_value: false };
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    Sorted(value).serialize(&mut serializer).expect("JSON values encode into memory");
    out
}

/// `T` as it serializes, but every map with its keys sorted; structs and sequences stream
/// through untouched, only a map's entries wait (as values) until all are known.
struct Sorted<'a, T: ?Sized>(&'a T);

impl<T: Serialize + ?Sized> Serialize for Sorted<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(Sorting(serializer))
    }
}

struct Sorting<S>(S);

macro_rules! forward {
    ($($method:ident($type:ty)),* $(,)?) => {
        $(fn $method(self, value: $type) -> Result<S::Ok, S::Error> {
            self.0.$method(value)
        })*
    };
}

impl<S: Serializer> Serializer for Sorting<S> {
    type Ok = S::Ok;
    type Error = S::Error;
    type SerializeSeq = Each<S::SerializeSeq>;
    type SerializeTuple = Each<S::SerializeTuple>;
    type SerializeTupleStruct = Each<S::SerializeTupleStruct>;
    type SerializeTupleVariant = Each<S::SerializeTupleVariant>;
    type SerializeMap = SortedMap<S>;
    type SerializeStruct = Each<S::SerializeStruct>;
    type SerializeStructVariant = Each<S::SerializeStructVariant>;

    forward!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_f32(f32),
        serialize_f64(f64),
        serialize_char(char),
        serialize_str(&str),
        serialize_bytes(&[u8]),
        serialize_unit_struct(&'static str),
    );

    fn serialize_none(self) -> Result<S::Ok, S::Error> {
        self.0.serialize_none()
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<S::Ok, S::Error> {
        self.0.serialize_some(&Sorted(value))
    }

    fn serialize_unit(self) -> Result<S::Ok, S::Error> {
        self.0.serialize_unit()
    }

    fn serialize_unit_variant(self, name: &'static str, index: u32, variant: &'static str) -> Result<S::Ok, S::Error> {
        self.0.serialize_unit_variant(name, index, variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, name: &'static str, value: &T) -> Result<S::Ok, S::Error> {
        self.0.serialize_newtype_struct(name, &Sorted(value))
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<S::Ok, S::Error> {
        self.0.serialize_newtype_variant(name, index, variant, &Sorted(value))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, S::Error> {
        self.0.serialize_seq(len).map(Each)
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, S::Error> {
        self.0.serialize_tuple(len).map(Each)
    }

    fn serialize_tuple_struct(self, name: &'static str, len: usize) -> Result<Self::SerializeTupleStruct, S::Error> {
        self.0.serialize_tuple_struct(name, len).map(Each)
    }

    fn serialize_tuple_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, S::Error> {
        self.0.serialize_tuple_variant(name, index, variant, len).map(Each)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, S::Error> {
        Ok(SortedMap { serializer: self.0, entries: Vec::with_capacity(len.unwrap_or(0)), key: None })
    }

    fn serialize_struct(self, name: &'static str, len: usize) -> Result<Self::SerializeStruct, S::Error> {
        self.0.serialize_struct(name, len).map(Each)
    }

    fn serialize_struct_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, S::Error> {
        self.0.serialize_struct_variant(name, index, variant, len).map(Each)
    }

    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }
}

/// A sequence, tuple or struct whose elements go through `Sorted` as they come.
struct Each<C>(C);

impl<C: SerializeSeq> SerializeSeq for Each<C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        self.0.serialize_element(&Sorted(value))
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.0.end()
    }
}

impl<C: SerializeTuple> SerializeTuple for Each<C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        self.0.serialize_element(&Sorted(value))
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.0.end()
    }
}

impl<C: SerializeTupleStruct> SerializeTupleStruct for Each<C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        self.0.serialize_field(&Sorted(value))
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.0.end()
    }
}

impl<C: SerializeTupleVariant> SerializeTupleVariant for Each<C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        self.0.serialize_field(&Sorted(value))
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.0.end()
    }
}

impl<C: SerializeStruct> SerializeStruct for Each<C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), C::Error> {
        self.0.serialize_field(key, &Sorted(value))
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), C::Error> {
        self.0.skip_field(key)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.0.end()
    }
}

impl<C: SerializeStructVariant> SerializeStructVariant for Each<C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), C::Error> {
        self.0.serialize_field(key, &Sorted(value))
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), C::Error> {
        self.0.skip_field(key)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.0.end()
    }
}

/// A map's entries, held as values (their own maps already sorted) and written in key order.
struct SortedMap<S> {
    serializer: S,
    entries: Vec<(Value, Value)>,
    key: Option<Value>,
}

impl<S: Serializer> SerializeMap for SortedMap<S> {
    type Ok = S::Ok;
    type Error = S::Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), S::Error> {
        self.key = Some(held(key)?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), S::Error> {
        let key = self.key.take().expect("serde asks for a map's key before its value");
        self.entries.push((key, held(value)?));
        Ok(())
    }

    fn end(mut self) -> Result<S::Ok, S::Error> {
        self.entries.sort_by(|(a, _), (b, _)| key_text(a).cmp(&key_text(b)));
        let mut map = self.serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

fn held<T: Serialize + ?Sized, E: ser::Error>(value: &T) -> Result<Value, E> {
    serde_json::to_value(Sorted(value)).map_err(E::custom)
}

/// A key as JSON writes it: strings as they are, numbers and the rest as their text.
fn key_text(key: &Value) -> Cow<'_, str> {
    match key {
        Value::String(text) => Cow::Borrowed(text),
        other => Cow::Owned(other.to_string()),
    }
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
