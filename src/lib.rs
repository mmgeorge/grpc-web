use wasm_bindgen_futures::{JsFuture};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use web_sys::{RequestInit, Request, RequestMode, Response, window};
use js_sys::Uint8Array;
use serde_json;

use prost::Message;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum ParseError {
    SerdeError(String),
    DecodingError(prost::DecodeError),
    EncodingError(prost::EncodeError),
    BadFrameType,
    BadTrailer(std::str::Utf8Error),
    MissingFrameType,
    MissingLength,
    MissingMessage,
    MissingWindow,
    WebAPI(JsValue)
}

impl From<prost::DecodeError> for ParseError {
    fn from(error: prost::DecodeError) -> Self {
        ParseError::DecodingError(error)
    }
}

impl From<prost::EncodeError> for ParseError {
    fn from(error: prost::EncodeError) -> Self {
        ParseError::EncodingError(error)
    }
}

impl From<JsValue> for ParseError {
    fn from(error: JsValue) -> Self {
        ParseError::WebAPI(error)
    }
}

impl From <serde_json::Error> for ParseError {
    fn from(error: serde_json::Error) -> Self {
        ParseError::SerdeError(format!("{}", error))
    }
}

// MG: Todo - Improve error handling:
//   1. populate name and desc
//   2. simplify number of error types -> DecodingError, FetchError, etc
impl From<ParseError> for js_sys::Error {
    fn from (error: ParseError) -> Self {
        js_sys::Error::new(&format!("{:?}", error))
    }
}

impl From<ParseError> for JsValue {
    fn from (error: ParseError) -> Self {
        JsValue::from(js_sys::Error::new(&format!("{:?}", error)))
    }
}



enum FrameType {
    Data = 0x00,
    Trailer = 0x80
}

pub enum MessageFrame<T> {
    DataFrame(T),
    TrailerFrame(HashMap<String, String>)
}

pub struct UnaryClient {
    url: String,
    token: String,
}

impl UnaryClient {
    pub fn new(url: String, token: String) -> UnaryClient {
        UnaryClient { url, token }
    }
    
    pub async fn unary_call<T: Message + Default, G: Message>(&self, uri: &str, request: &G) -> Result<T, ParseError> {
        let url = [self.url.as_str(), uri].concat();
        let mut buf = vec![];
        
        request.encode(&mut buf)?;
        
        let bytes = encode(buf);
        let request = make_post(&url, bytes).map_err(|e| ParseError::WebAPI(e))?;
        let window = window().ok_or(ParseError::MissingWindow)?;
        let response = JsFuture::from(window.fetch_with_request(&request)).await?; 

        assert!(response.is_instance_of::<Response>());

        let resp: Response = response.dyn_into()?;
        let buf = JsFuture::from(resp.array_buffer()?).await?;

        // MG: Should check type of response, throw accordingly if not 200

        let  data = Uint8Array::new_with_byte_offset(&buf, 0).to_vec(); // This not ::new ctor required
        let mut iter = data.into_iter().peekable();

        while iter.peek().is_some() {
            if let Ok(MessageFrame::DataFrame::<T>(message)) = parse_message(&mut iter) {
                return Ok(message);
            }
        }

        Err(ParseError::MissingMessage)
    }    
}



fn make_post(url: &str, bytes: Vec<u8>) -> Result<Request, JsValue> {
    let arr = Uint8Array::from(bytes.as_slice());
    let mut options = RequestInit::new();

    options.method("POST");
    options.mode(RequestMode::Cors);
    options.body(Some(arr.as_ref()));

    let request = Request::new_with_str_and_init(&url, &options)?;
    let headers = request.headers();

    headers.set("Accept", "application/grpc-web+proto")?;
    headers.set("Content-Type", "application/grpc-web+proto")?;
    headers.set("X-User-Agent", "grpc-web-owny")?;
    headers.set("X-Grpc-Web", "1")?;
    // headers.set("grpc-timeout", timeout + "m")?;
    
    Ok(request)
}


fn encode(bytes: Vec<u8>) -> Vec<u8> {
    let mut len = bytes.len();
    let mut bytes_array: Vec<u8> = vec![0, 0, 0, 0];
    let mut payload = Vec::<u8>::new();
    
    for i in (0..4).rev() {
        bytes_array[i] = (len % 256) as u8;
        len = len >> 8;
    }

    payload.push(0);
    payload.push(bytes_array[0]);
    payload.push(bytes_array[1]);
    payload.push(bytes_array[2]);
    payload.push(bytes_array[3]);

    payload.append(&mut bytes.clone());
    
    return payload;
}

pub fn parse_message<I: Iterator<Item = u8> , T: Message + Default>(mut iter: I) -> Result<MessageFrame<T>, ParseError> {
    let frame_type = match iter.next().ok_or(ParseError::MissingFrameType)? {
        0x00 => FrameType::Data,
        0x80 => FrameType::Trailer,
        _ => return Err(ParseError::BadFrameType)
    };

    // Unpack message length
    let mut len: usize = 0;

    for _ in 0..4 {
        len = (len << 8) + iter.next().ok_or(ParseError::MissingLength)? as usize;
    }

    match frame_type {
        FrameType::Data => parse_data_frame(iter.take(len)),
        FrameType::Trailer => parse_trailer_frame(iter.take(len))
    }
}


fn parse_data_frame<I: Iterator<Item = u8>, T: Message + Default>(iter: I) -> Result<MessageFrame<T>, ParseError> {
    let buf: Vec<u8> = iter.collect();

    Message::decode(buf.as_slice())
        .map(|x| MessageFrame::DataFrame(x))
        .map_err(|e| ParseError::DecodingError(e))
}


fn parse_trailer_frame<I, T: Message>(iter: I) -> Result<MessageFrame<T>, ParseError>  where I: Iterator<Item = u8>{
    let buf: Vec<u8> = iter.collect();
    let trailers = std::str::from_utf8(buf.as_slice()).map_err(|e| ParseError::BadTrailer(e))?;

    let mut out: HashMap<String, String> = HashMap::new();

    for header in trailers.split("\r\n") {
        let mut iter = header.split(":");
        let key = iter.next();
        let value = iter.next();

        if let (Some(key), Some(value)) = (key, value) {
            out.insert(key.to_string(), value.to_string());
        }
    }

   Ok(MessageFrame::TrailerFrame(out))
}
