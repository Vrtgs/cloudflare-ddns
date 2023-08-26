use serde::{Deserialize};



#[derive(Debug, Deserialize)]
pub struct Record {
    pub id: String,
    pub name: String,
    #[serde(rename = "content")]
    pub ip: String,
}

#[derive(Debug, Deserialize)]
pub struct GetResponse {
    pub result: Vec<Record>
}

#[derive(Debug, Deserialize)]
pub struct PatchResponse {
    pub success: bool,
}