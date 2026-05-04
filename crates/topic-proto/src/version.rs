use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Marker that serializes/deserializes as the literal string `"2.0"`.
///
/// Using a marker type instead of `String` makes wire compliance a
/// type-system property: a `Request` whose `jsonrpc` field has the
/// wrong version will fail to deserialize, with no runtime check
/// required at the dispatcher.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl JsonRpcVersion {
  pub const STR: &'static str = "2.0";
}

impl Serialize for JsonRpcVersion {
  fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(Self::STR)
  }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
  fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
    let raw = String::deserialize(de)?;
    if raw == Self::STR {
      Ok(JsonRpcVersion)
    } else {
      Err(de::Error::custom(format!(
        "expected jsonrpc version {:?}, got {raw:?}",
        Self::STR
      )))
    }
  }
}
