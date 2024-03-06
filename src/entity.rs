use std::fmt::{Debug, Display, Formatter};
use std::ops::Deref;
use simd_json::Node;
use simd_json_derive::{Deserialize, Tape};


pub struct OwnedStr(Box<str>);

impl<'input> Deserialize<'input> for OwnedStr {
    fn from_tape(tape: &mut Tape<'input>) -> simd_json::Result<Self> where Self: Sized + 'input {
        match tape.next() {
            Some(Node::String(s)) => Ok(Self(Box::from(s))),
            _ => Err(simd_json::Error::generic(
                simd_json::ErrorType::ExpectedString
            )),
        }
    }
}
impl Deref for OwnedStr {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        Box::deref(&self.0)
    }
}


impl Debug for OwnedStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        <str as Debug>::fmt(self, f)
    }
}

impl Display for OwnedStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        <str as Display>::fmt(self, f)
    }
}

#[derive(Debug, Deserialize)]
pub struct Record {
    pub id: OwnedStr,
    pub name: OwnedStr,
    #[serde(rename = "content")]
    pub ip: OwnedStr,
}

#[derive(Debug)]
pub enum OneOrLen<T> {
    One(T),
    Len(usize)
}

impl<'input, T: Deserialize<'input>> Deserialize<'input> for OneOrLen<T> {
    fn from_tape(tape: &mut Tape<'input>) -> simd_json::Result<Self> where Self: Sized + 'input {
        match tape.next() {
            Some(Node::Array { len: 1, .. }) => T::from_tape(tape).map(OneOrLen::One),
            Some(Node::Array { len, .. }) => Ok(OneOrLen::Len(len)),
            _ => Err(simd_json::Error::generic(
                simd_json::ErrorType::ExpectedArray
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct GetResponse {
    pub result: OneOrLen<Record>
}

#[derive(Debug, Deserialize)]
pub struct PatchResponse {
    pub success: bool
}