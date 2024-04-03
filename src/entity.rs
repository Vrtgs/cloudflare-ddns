use std::fmt;
use std::fmt::{Debug, Formatter};
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use serde::{Deserialize, Deserializer};
use serde::de::{SeqAccess, Visitor};


#[derive(Debug, Deserialize)]
pub struct Record {
    pub id: Box<str>,
    pub name: Box<str>,
    #[serde(rename = "content")]
    pub ip: Box<str>,
}

#[derive(Debug)]
pub enum OneOrLen<T> {
    One(T),
    Len(usize)
}

impl<'input, T: Deserialize<'input>> Deserialize<'input> for OneOrLen<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'input> {
        struct OneOrLenVisitor<T> {
            marker: PhantomData<T>
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
                
                match last_element.map(|(cnt, x)| (cnt.get(), x)) {
                    Some((1, element)) => Ok(OneOrLen::One(element)),
                    Some((cnt, _)) => Ok(OneOrLen::Len(cnt)),
                    None => Ok(OneOrLen::Len(0))
                }
            }
        }

        deserializer.deserialize_seq(OneOrLenVisitor { marker: PhantomData })
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