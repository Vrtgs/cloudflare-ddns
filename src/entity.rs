use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ops::Deref;
use serde::{Deserialize, Deserializer};
use serde::de::{Error, SeqAccess, Visitor};


pub struct OwnedStr(Box<str>);
pub struct OwnedBytes(Box<[u8]>);

impl<'de> Deserialize<'de> for OwnedStr {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
        String::deserialize(deserializer).map(String::into_boxed_str).map(OwnedStr)
    }
}

impl<'de> Deserialize<'de> for OwnedBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
        struct BytesVisitor {
            marker: PhantomData<[u8]>,
        }

        impl<'de> Visitor<'de> for BytesVisitor {
            type Value = OwnedBytes;

            fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
                formatter.write_str("bytes")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E> where E: Error {
                Ok(OwnedBytes(Box::from(v)))
            }
            
            #[inline(always)]
            fn visit_borrowed_bytes<E>(self, v: &'de [u8]) -> Result<Self::Value, E> where E: Error {
                self.visit_bytes(v)
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E> where E: Error {
                Ok(OwnedBytes(v.into_boxed_slice()))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let capacity = seq.size_hint().map(|x| x.min(2 * 1024 * 1024))
                    .unwrap_or(0);
                let mut values = Vec::with_capacity(capacity);

                while let Some(value) = seq.next_element::<u8>()? {
                    values.push(value);
                }

                Ok(OwnedBytes(values.into_boxed_slice()))
            }
        }

        let visitor = BytesVisitor { marker: PhantomData };
        
        deserializer.deserialize_byte_buf(visitor)
    }
}


impl Deref for OwnedBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Debug for OwnedBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        <[u8] as Debug>::fmt(self, f)
    }
}

impl Deref for OwnedStr {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}


impl Debug for OwnedStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        <str as Debug>::fmt(self, f)
    }
}

impl Display for OwnedStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
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
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'input> {
        struct OneOrLenVisitor<T> {
            marker: PhantomData<T>,
        }

        impl<'de, T: Deserialize<'de>> Visitor<'de> for OneOrLenVisitor<T> {
            type Value = OneOrLen<T>;

            fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut last_element: Option<(NonZeroUsize, T)> = None;
                while let Some(element) = seq.next_element::<T>()? {
                    last_element = match last_element {
                        None => Some((NonZeroUsize::MIN, element)),
                        Some((cnt, _)) => Some((cnt.saturating_add(1), element))
                    };
                }
                
                match last_element {
                    Some((cnt, element)) if cnt.get() == 1 => Ok(OneOrLen::One(element)),
                    Some((cnt, _)) => Ok(OneOrLen::Len(cnt.get())),
                    None => Ok(OneOrLen::Len(0))
                }
            }
        }

        let visitor = OneOrLenVisitor {
            marker: PhantomData,
        };
        deserializer.deserialize_seq(visitor)
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