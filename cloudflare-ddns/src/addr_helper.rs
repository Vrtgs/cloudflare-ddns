use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::net;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AddrParseError {
    #[error("The input data was too long to even be considered an address")]
    TooLong,
    #[error("invalid encoding on the addresses bytes")]
    InvalidEncoding,
    #[error(transparent)]
    Parse(#[from] net::AddrParseError),
}

pub trait AddrParseExt: Sized {
    fn parse_ascii_bytes(b: &[u8]) -> Result<Self, AddrParseError>;
}

macro_rules! test_gen {
    (Ipv4Addr => $item:item) => {
        #[cfg(test)]
        mod ip_v4_max_test {
            use super::*;
            #[test]
            $item
        }
    };
    (Ipv6Addr => $item:item) => {
        #[cfg(test)]
        mod ip_v6_max_test {
            use super::*;
            #[test]
            $item
        }
    };
}

macro_rules! impl_addr {
    ($ty: ident, max: $biggest_addr:literal) => {
        impl AddrParseExt for $ty {
            fn parse_ascii_bytes(b: &[u8]) -> Result<Self, AddrParseError> {
                let b = b.trim_ascii();

                if b.len() > ($biggest_addr).len() {
                    return Err(AddrParseError::TooLong);
                }

                b.is_ascii()
                    .then(|| unsafe { std::str::from_utf8_unchecked(b) })
                    .ok_or(AddrParseError::InvalidEncoding)
                    .and_then(|s| <$ty>::from_str(s).map_err(Into::into))
            }
        }

        test_gen! {
            $ty => fn test_ip_max() {
                assert_eq!(
                    ($biggest_addr).len(),
                    <$ty>::from(std::array::from_fn(|_| u8::MAX)).to_string().len()
                )
            }
        }
    };
}

impl_addr! {
    Ipv4Addr,
    max: b"xxx.xxx.xxx.xxx"
}

impl_addr! {
    Ipv6Addr,
    max: b"xxxx:xxxx:xxxx:xxxx:xxxx:xxxx:xxxx:xxxx"
}

impl AddrParseExt for IpAddr {
    fn parse_ascii_bytes(bytes: &[u8]) -> Result<Self, AddrParseError> {
        Ipv4Addr::parse_ascii_bytes(bytes)
            .map(IpAddr::V4)
            .or_else(|_| Ipv6Addr::parse_ascii_bytes(bytes).map(IpAddr::V6))
    }
}

#[derive(Ord, PartialOrd, Eq, PartialEq, Copy, Clone, Debug, Default)]
pub enum IpUpdateType {
    Any,
    Both,
    V6,
    #[default]
    V4,
}

#[derive(Ord, PartialOrd, Eq, PartialEq, Copy, Clone, Debug, Default)]
pub enum IpType {
    Any,
    V6,
    #[default]
    V4,
}

impl From<IpType> for IpUpdateType {
    fn from(value: IpType) -> Self {
        match value {
            IpType::Any => Self::Any,
            IpType::V6 => Self::V6,
            IpType::V4 => Self::V4,
        }
    }
}

impl<'de> Deserialize<'de> for IpType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).and_then(|mut str| {
            str.make_ascii_lowercase();

            match str.as_str() {
                "any" => Ok(Self::Any),
                "v4" | "ipv4" => Ok(Self::V4),
                "v6" | "ipv6" => Ok(Self::V6),
                ty => Err(Error::custom(format_args!("unknown ip type {ty}"))),
            }
        })
    }
}

impl Serialize for IpType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let str = match *self {
            IpType::Any => "any",
            IpType::V6 => "ipv4",
            IpType::V4 => "ipv6",
        };

        str::serialize(str, serializer)
    }
}

impl<'de> Deserialize<'de> for IpUpdateType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).and_then(|mut str| {
            str.make_ascii_lowercase();

            match str.as_str() {
                "any" => Ok(Self::Any),
                "both" => Ok(Self::Both),
                "v4" | "ipv4" => Ok(Self::V4),
                "v6" | "ipv6" => Ok(Self::V6),
                ty => Err(Error::custom(format_args!("unknown ip update type {ty}"))),
            }
        })
    }
}
