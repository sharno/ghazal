//! A blocking D-Bus client with just enough of the wire protocol for BlueZ: method
//! calls, replies, and an object that answers calls made to us (BlueZ's pairing
//! agent). A general D-Bus crate would add an async runtime to a binary whose
//! page-in time is part of every boot, for a screen that opens rarely.
//!
//! Messages are little-endian only (the daemon speaks the host's byte order, and
//! the handheld is little-endian). File descriptor passing is not negotiated.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};

/// The specification allows 128 MiB; BlueZ's largest reply here is a few KiB.
const MAX_MESSAGE: usize = 1 << 20;
/// One read waits this long, so a caller notices an abort request between slices.
const SLICE: Duration = Duration::from_millis(100);
const MAX_NESTING: usize = 32;
const NO_REPLY_EXPECTED: u8 = 1;

/// A D-Bus type, parsed from a signature.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Byte,
    Bool,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F64,
    Str,
    Path,
    Sig,
    Variant,
    Array(Box<Ty>),
    Struct(Vec<Ty>),
    Entry(Box<Ty>, Box<Ty>),
}

impl Ty {
    fn align(&self) -> usize {
        match self {
            Self::Byte | Self::Sig | Self::Variant => 1,
            Self::I16 | Self::U16 => 2,
            Self::Bool | Self::I32 | Self::U32 | Self::Str | Self::Path | Self::Array(_) => 4,
            Self::I64 | Self::U64 | Self::F64 | Self::Struct(_) | Self::Entry(..) => 8,
        }
    }

    pub fn parse_all(signature: &str) -> Result<Vec<Self>> {
        let mut chars = signature.bytes().peekable();
        let mut types = Vec::new();
        while chars.peek().is_some() {
            types.push(Self::parse_one(&mut chars, 0)?);
        }
        Ok(types)
    }

    fn parse_one(chars: &mut std::iter::Peekable<std::str::Bytes>, depth: usize) -> Result<Self> {
        ensure!(depth < MAX_NESTING, "signature nests too deeply");
        let character = chars.next().context("truncated signature")?;
        Ok(match character {
            b'y' => Self::Byte,
            b'b' => Self::Bool,
            b'n' => Self::I16,
            b'q' => Self::U16,
            b'i' => Self::I32,
            b'u' => Self::U32,
            b'x' => Self::I64,
            b't' => Self::U64,
            b'd' => Self::F64,
            b's' => Self::Str,
            b'o' => Self::Path,
            b'g' => Self::Sig,
            b'v' => Self::Variant,
            b'a' => Self::Array(Box::new(Self::parse_one(chars, depth + 1)?)),
            b'(' => {
                let mut fields = Vec::new();
                while chars.peek() != Some(&b')') {
                    fields.push(Self::parse_one(chars, depth + 1)?);
                }
                chars.next();
                ensure!(!fields.is_empty(), "empty struct in signature");
                Self::Struct(fields)
            }
            b'{' => {
                let key = Self::parse_one(chars, depth + 1)?;
                let value = Self::parse_one(chars, depth + 1)?;
                ensure!(chars.next() == Some(b'}'), "unterminated dictionary entry in signature");
                Self::Entry(Box::new(key), Box::new(value))
            }
            other => bail!("unsupported signature character {:?}", other as char),
        })
    }

    pub fn signature(&self) -> String {
        match self {
            Self::Byte => "y".into(),
            Self::Bool => "b".into(),
            Self::I16 => "n".into(),
            Self::U16 => "q".into(),
            Self::I32 => "i".into(),
            Self::U32 => "u".into(),
            Self::I64 => "x".into(),
            Self::U64 => "t".into(),
            Self::F64 => "d".into(),
            Self::Str => "s".into(),
            Self::Path => "o".into(),
            Self::Sig => "g".into(),
            Self::Variant => "v".into(),
            Self::Array(element) => format!("a{}", element.signature()),
            Self::Struct(fields) => format!("({})", fields.iter().map(Self::signature).collect::<String>()),
            Self::Entry(key, value) => format!("{{{}{}}}", key.signature(), value.signature()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    Path(String),
    Sig(String),
    Variant(Box<Value>),
    /// The element type is kept so an empty array still has a signature.
    Array(Ty, Vec<Value>),
    Struct(Vec<Value>),
    Entry(Box<Value>, Box<Value>),
}

impl Value {
    pub fn str(text: &str) -> Self {
        Self::Str(text.to_owned())
    }

    pub fn path(text: &str) -> Self {
        Self::Path(text.to_owned())
    }

    pub fn variant(value: Self) -> Self {
        Self::Variant(Box::new(value))
    }

    pub fn ty(&self) -> Ty {
        match self {
            Self::Byte(_) => Ty::Byte,
            Self::Bool(_) => Ty::Bool,
            Self::I16(_) => Ty::I16,
            Self::U16(_) => Ty::U16,
            Self::I32(_) => Ty::I32,
            Self::U32(_) => Ty::U32,
            Self::I64(_) => Ty::I64,
            Self::U64(_) => Ty::U64,
            Self::F64(_) => Ty::F64,
            Self::Str(_) => Ty::Str,
            Self::Path(_) => Ty::Path,
            Self::Sig(_) => Ty::Sig,
            Self::Variant(_) => Ty::Variant,
            Self::Array(element, _) => Ty::Array(Box::new(element.clone())),
            Self::Struct(fields) => Ty::Struct(fields.iter().map(Self::ty).collect()),
            Self::Entry(key, value) => Ty::Entry(Box::new(key.ty()), Box::new(value.ty())),
        }
    }

    /// The value inside any number of variant wrappers.
    pub fn peeled(&self) -> &Self {
        match self {
            Self::Variant(inner) => inner.peeled(),
            other => other,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self.peeled() {
            Self::Str(text) | Self::Path(text) | Self::Sig(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.peeled() {
            Self::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self.peeled() {
            Self::U32(number) => Some(*number),
            _ => None,
        }
    }

    pub fn as_i16(&self) -> Option<i16> {
        match self.peeled() {
            Self::I16(number) => Some(*number),
            _ => None,
        }
    }

    pub fn as_u16(&self) -> Option<u16> {
        match self.peeled() {
            Self::U16(number) => Some(*number),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Self]> {
        match self.peeled() {
            Self::Array(_, items) => Some(items),
            _ => None,
        }
    }

    /// The key/value pairs of a dictionary (`a{..}`).
    pub fn entries(&self) -> impl Iterator<Item = (&Self, &Self)> {
        self.as_array().unwrap_or(&[]).iter().filter_map(|item| match item {
            Self::Entry(key, value) => Some((key.as_ref(), value.as_ref())),
            _ => None,
        })
    }
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn align(&mut self, boundary: usize) {
        while !self.bytes.len().is_multiple_of(boundary) {
            self.bytes.push(0);
        }
    }

    fn fixed(&mut self, boundary: usize, bytes: &[u8]) {
        self.align(boundary);
        self.bytes.extend_from_slice(bytes);
    }

    fn u32(&mut self, number: u32) {
        self.fixed(4, &number.to_le_bytes());
    }

    fn string(&mut self, text: &str) {
        self.u32(text.len() as u32);
        self.bytes.extend_from_slice(text.as_bytes());
        self.bytes.push(0);
    }

    fn signature(&mut self, text: &str) {
        self.bytes.push(text.len() as u8);
        self.bytes.extend_from_slice(text.as_bytes());
        self.bytes.push(0);
    }

    fn value(&mut self, value: &Value) {
        match value {
            Value::Byte(number) => self.bytes.push(*number),
            Value::Bool(flag) => self.u32(u32::from(*flag)),
            Value::I16(number) => self.fixed(2, &number.to_le_bytes()),
            Value::U16(number) => self.fixed(2, &number.to_le_bytes()),
            Value::I32(number) => self.fixed(4, &number.to_le_bytes()),
            Value::U32(number) => self.u32(*number),
            Value::I64(number) => self.fixed(8, &number.to_le_bytes()),
            Value::U64(number) => self.fixed(8, &number.to_le_bytes()),
            Value::F64(number) => self.fixed(8, &number.to_le_bytes()),
            Value::Str(text) | Value::Path(text) => self.string(text),
            Value::Sig(text) => self.signature(text),
            Value::Variant(inner) => {
                self.signature(&inner.ty().signature());
                self.value(inner);
            }
            Value::Array(element, items) => {
                self.u32(0);
                let length_at = self.bytes.len() - 4;
                // The length excludes the padding that precedes the first element.
                self.align(element.align());
                let start = self.bytes.len();
                for item in items {
                    self.value(item);
                }
                let length = (self.bytes.len() - start) as u32;
                self.bytes[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());
            }
            Value::Struct(fields) => {
                self.align(8);
                for field in fields {
                    self.value(field);
                }
            }
            Value::Entry(key, value) => {
                self.align(8);
                self.value(key);
                self.value(value);
            }
        }
    }
}

/// Reads values at their absolute position in the message, since alignment
/// counts from the start of the message.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn align(&mut self, boundary: usize) -> Result<()> {
        self.at = self.at.div_ceil(boundary) * boundary;
        ensure!(self.at <= self.bytes.len(), "message ends inside padding");
        Ok(())
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(count).filter(|end| *end <= self.bytes.len()).context("message is truncated")?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self, boundary: usize) -> Result<[u8; N]> {
        self.align(boundary)?;
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.take(N)?);
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array(4)?))
    }

    fn string(&mut self) -> Result<String> {
        let length = self.u32()? as usize;
        let text = String::from_utf8(self.take(length)?.to_vec()).context("string is not UTF-8")?;
        ensure!(self.take(1)? == [0], "string is not terminated");
        Ok(text)
    }

    fn signature(&mut self) -> Result<String> {
        let length = usize::from(self.take(1)?[0]);
        let text = String::from_utf8(self.take(length)?.to_vec()).context("signature is not UTF-8")?;
        ensure!(self.take(1)? == [0], "signature is not terminated");
        Ok(text)
    }

    fn value(&mut self, ty: &Ty) -> Result<Value> {
        Ok(match ty {
            Ty::Byte => Value::Byte(self.take(1)?[0]),
            Ty::Bool => Value::Bool(self.u32()? != 0),
            Ty::I16 => Value::I16(i16::from_le_bytes(self.array(2)?)),
            Ty::U16 => Value::U16(u16::from_le_bytes(self.array(2)?)),
            Ty::I32 => Value::I32(i32::from_le_bytes(self.array(4)?)),
            Ty::U32 => Value::U32(self.u32()?),
            Ty::I64 => Value::I64(i64::from_le_bytes(self.array(8)?)),
            Ty::U64 => Value::U64(u64::from_le_bytes(self.array(8)?)),
            Ty::F64 => Value::F64(f64::from_le_bytes(self.array(8)?)),
            Ty::Str => Value::Str(self.string()?),
            Ty::Path => Value::Path(self.string()?),
            Ty::Sig => Value::Sig(self.signature()?),
            Ty::Variant => {
                let signature = self.signature()?;
                let mut types = Ty::parse_all(&signature)?;
                ensure!(types.len() == 1, "variant holds {} values", types.len());
                Value::variant(self.value(&types.remove(0))?)
            }
            Ty::Array(element) => {
                let length = self.u32()? as usize;
                self.align(element.align())?;
                let end = self.at.checked_add(length).filter(|end| *end <= self.bytes.len()).context("array is truncated")?;
                let mut items = Vec::new();
                while self.at < end {
                    items.push(self.value(element)?);
                }
                ensure!(self.at == end, "array elements overrun the array");
                Value::Array((**element).clone(), items)
            }
            Ty::Struct(fields) => {
                self.align(8)?;
                Value::Struct(fields.iter().map(|field| self.value(field)).collect::<Result<_>>()?)
            }
            Ty::Entry(key, value) => {
                self.align(8)?;
                Value::Entry(Box::new(self.value(key)?), Box::new(self.value(value)?))
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodCall {
    pub destination: String,
    pub path: String,
    pub interface: String,
    pub member: String,
    pub body: Vec<Value>,
}

/// A call made to an object we serve.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingCall {
    pub serial: u32,
    pub sender: String,
    pub path: String,
    pub interface: Option<String>,
    pub member: String,
    pub body: Vec<Value>,
    pub expects_reply: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Return(Vec<Value>),
    Error { name: String, text: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Call(IncomingCall),
    Return { reply_serial: u32, body: Vec<Value> },
    Error { reply_serial: u32, name: String, text: String },
    Signal,
}

const CALL: u8 = 1;
const RETURN: u8 = 2;
const ERROR: u8 = 3;

const PATH: u8 = 1;
const INTERFACE: u8 = 2;
const MEMBER: u8 = 3;
const ERROR_NAME: u8 = 4;
const REPLY_SERIAL: u8 = 5;
const DESTINATION: u8 = 6;
const SENDER: u8 = 7;
const SIGNATURE: u8 = 8;

fn field(code: u8, value: Value) -> Value {
    Value::Struct(vec![Value::Byte(code), Value::variant(value)])
}

fn encode(kind: u8, serial: u32, mut fields: Vec<Value>, body: &[Value]) -> Vec<u8> {
    let mut payload = Writer { bytes: Vec::new() };
    for value in body {
        payload.value(value);
    }
    if !body.is_empty() {
        fields.push(field(SIGNATURE, Value::Sig(body.iter().map(|value| value.ty().signature()).collect())));
    }
    let mut message = Writer { bytes: vec![b'l', kind, 0, 1] };
    message.u32(payload.bytes.len() as u32);
    message.u32(serial);
    message.value(&Value::Array(Ty::Struct(vec![Ty::Byte, Ty::Variant]), fields));
    message.align(8);
    message.bytes.extend_from_slice(&payload.bytes);
    message.bytes
}

fn encode_call(serial: u32, call: &MethodCall) -> Vec<u8> {
    let fields = vec![
        field(PATH, Value::path(&call.path)),
        field(DESTINATION, Value::str(&call.destination)),
        field(INTERFACE, Value::str(&call.interface)),
        field(MEMBER, Value::str(&call.member)),
    ];
    encode(CALL, serial, fields, &call.body)
}

fn encode_reply(serial: u32, to: &IncomingCall, reply: &Reply) -> Vec<u8> {
    let addressed = [field(REPLY_SERIAL, Value::U32(to.serial)), field(DESTINATION, Value::str(&to.sender))];
    match reply {
        Reply::Return(body) => encode(RETURN, serial, addressed.to_vec(), body),
        Reply::Error { name, text } => {
            let mut fields = addressed.to_vec();
            fields.push(field(ERROR_NAME, Value::str(name)));
            encode(ERROR, serial, fields, &[Value::str(text)])
        }
    }
}

/// Total length of the message at the start of `bytes`, once its fixed header has arrived.
fn message_length(bytes: &[u8]) -> Result<Option<usize>> {
    let Some(header) = bytes.get(..16) else { return Ok(None) };
    ensure!(header[0] == b'l', "big-endian D-Bus messages are not supported");
    ensure!(header[3] == 1, "unsupported D-Bus protocol version {}", header[3]);
    let body = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let fields = u32::from_le_bytes([header[12], header[13], header[14], header[15]]) as usize;
    let total = (16 + fields).div_ceil(8) * 8 + body;
    ensure!(total <= MAX_MESSAGE, "D-Bus message of {total} bytes is too large");
    Ok(Some(total))
}

fn decode(bytes: &[u8]) -> Result<Message> {
    let mut reader = Reader { bytes, at: 0 };
    let [_, kind, flags, _] = reader.array(1)?;
    let body_length = reader.u32()? as usize;
    let serial = reader.u32()?;
    let fields = reader.value(&Ty::Array(Box::new(Ty::Struct(vec![Ty::Byte, Ty::Variant]))))?;
    reader.align(8)?;
    ensure!(bytes.len() - reader.at == body_length, "body length does not match the message");

    let text = |code: u8| {
        fields.as_array().unwrap_or(&[]).iter().find_map(|item| match item {
            Value::Struct(parts) => match parts.as_slice() {
                [Value::Byte(found), value] if *found == code => value.as_str().map(str::to_owned),
                _ => None,
            },
            _ => None,
        })
    };
    let reply_serial = fields.as_array().unwrap_or(&[]).iter().find_map(|item| match item {
        Value::Struct(parts) => match parts.as_slice() {
            [Value::Byte(REPLY_SERIAL), value] => value.as_u32(),
            _ => None,
        },
        _ => None,
    });
    let signature = text(SIGNATURE).unwrap_or_default();
    let mut body = Vec::new();
    if kind != 4 {
        for ty in Ty::parse_all(&signature)? {
            body.push(reader.value(&ty)?);
        }
    }
    Ok(match kind {
        CALL => Message::Call(IncomingCall {
            serial,
            sender: text(SENDER).unwrap_or_default(),
            path: text(PATH).context("method call without a path")?,
            interface: text(INTERFACE),
            member: text(MEMBER).context("method call without a member")?,
            body,
            expects_reply: flags & NO_REPLY_EXPECTED == 0,
        }),
        RETURN => Message::Return { reply_serial: reply_serial.context("reply without a serial")?, body },
        ERROR => Message::Error {
            reply_serial: reply_serial.context("error without a serial")?,
            name: text(ERROR_NAME).unwrap_or_default(),
            text: body.first().and_then(Value::as_str).unwrap_or_default().to_owned(),
        },
        _ => Message::Signal,
    })
}

#[derive(Debug)]
pub enum CallError {
    /// The peer answered with a D-Bus error.
    Remote { name: String, text: String },
    Timeout,
    Aborted,
    Io(anyhow::Error),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Remote { name, text } if text.is_empty() => write!(formatter, "{name}"),
            Self::Remote { name, text } => write!(formatter, "{name}: {text}"),
            Self::Timeout => write!(formatter, "no reply from the Bluetooth service"),
            Self::Aborted => write!(formatter, "cancelled"),
            Self::Io(error) => write!(formatter, "{error:#}"),
        }
    }
}

impl std::error::Error for CallError {}

/// What the connection hands calls to, and how a blocked call is interrupted.
pub trait Serve {
    fn call(&mut self, call: &IncomingCall) -> Reply;
    fn aborted(&self) -> bool;
}

/// Serves nothing: every call to us is an unknown method.
pub struct Deaf;

impl Serve for Deaf {
    fn call(&mut self, _: &IncomingCall) -> Reply {
        Reply::Error { name: "org.freedesktop.DBus.Error.UnknownMethod".into(), text: String::new() }
    }

    fn aborted(&self) -> bool {
        false
    }
}

pub struct Connection {
    stream: UnixStream,
    serial: u32,
    name: String,
    inbox: Vec<u8>,
}

impl Connection {
    /// Connects, authenticates as the current user and says hello to the bus.
    pub fn open(socket: &Path) -> Result<Self> {
        let mut stream = UnixStream::connect(socket).with_context(|| format!("connecting to {}", socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() }.to_string();
        let hex: String = uid.bytes().map(|byte| format!("{byte:02x}")).collect();
        stream.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
        let reply = read_line(&mut stream)?;
        ensure!(reply.starts_with("OK "), "the D-Bus daemon refused authentication: {reply:?}");
        stream.write_all(b"BEGIN\r\n")?;
        stream.set_read_timeout(Some(SLICE))?;
        let mut connection = Self { stream, serial: 0, name: String::new(), inbox: Vec::new() };
        let hello = MethodCall {
            destination: "org.freedesktop.DBus".into(),
            path: "/org/freedesktop/DBus".into(),
            interface: "org.freedesktop.DBus".into(),
            member: "Hello".into(),
            body: Vec::new(),
        };
        let reply = connection.call(&hello, Duration::from_secs(5), &mut Deaf)?;
        connection.name = reply.first().and_then(Value::as_str).context("Hello returned no name")?.to_owned();
        Ok(connection)
    }

    /// Our unique name on the bus.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Calls a method and waits for its reply, answering calls made to us meanwhile.
    pub fn call(&mut self, call: &MethodCall, timeout: Duration, serve: &mut dyn Serve) -> Result<Vec<Value>, CallError> {
        self.serial += 1;
        let serial = self.serial;
        self.stream.write_all(&encode_call(serial, call)).map_err(|error| CallError::Io(error.into()))?;
        let deadline = Instant::now() + timeout;
        loop {
            if serve.aborted() {
                return Err(CallError::Aborted);
            }
            if Instant::now() >= deadline {
                return Err(CallError::Timeout);
            }
            match self.receive().map_err(CallError::Io)? {
                Some(Message::Return { reply_serial, body }) if reply_serial == serial => return Ok(body),
                Some(Message::Error { reply_serial, name, text }) if reply_serial == serial => {
                    return Err(CallError::Remote { name, text });
                }
                Some(Message::Call(incoming)) => self.answer(&incoming, serve).map_err(CallError::Io)?,
                // Signals, and replies to calls we gave up on.
                Some(_) | None => {}
            }
        }
    }

    /// Answers calls made to us for `duration`.
    pub fn serve_for(&mut self, duration: Duration, serve: &mut dyn Serve) -> Result<()> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if let Some(Message::Call(incoming)) = self.receive()? {
                self.answer(&incoming, serve)?;
            }
        }
        Ok(())
    }

    fn answer(&mut self, incoming: &IncomingCall, serve: &mut dyn Serve) -> Result<()> {
        let reply = serve.call(incoming);
        if !incoming.expects_reply {
            return Ok(());
        }
        self.serial += 1;
        self.stream.write_all(&encode_reply(self.serial, incoming, &reply))?;
        Ok(())
    }

    /// The next whole message, or None if nothing complete arrived within a slice.
    fn receive(&mut self) -> Result<Option<Message>> {
        loop {
            if let Some(length) = message_length(&self.inbox)? {
                if self.inbox.len() >= length {
                    let raw: Vec<u8> = self.inbox.drain(..length).collect();
                    return decode(&raw).map(Some);
                }
            }
            let mut chunk = [0; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => bail!("the D-Bus connection closed"),
                Ok(count) => self.inbox.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(None),
                Err(error) => return Err(anyhow!(error).context("reading from the D-Bus daemon")),
            }
        }
    }
}

/// One CRLF-terminated line, read byte by byte so nothing after it is consumed.
fn read_line(stream: &mut UnixStream) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0];
    while !line.ends_with(b"\r\n") {
        ensure!(line.len() < 1024, "D-Bus authentication line is too long");
        stream.read_exact(&mut byte).context("reading the D-Bus authentication reply")?;
        line.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&line).trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(value: &Value) -> Vec<u8> {
        let mut writer = Writer { bytes: Vec::new() };
        writer.value(value);
        writer.bytes
    }

    #[test]
    fn strings_variants_and_booleans_align_as_the_specification_says() {
        // "ab" (4+3), padding to 8, "c" (4+2), the variant's signature "b" (3),
        // padding to 20, then the boolean as a 32-bit integer.
        let mut writer = Writer { bytes: Vec::new() };
        for value in [Value::str("ab"), Value::str("c"), Value::variant(Value::Bool(true))] {
            writer.value(&value);
        }
        assert_eq!(
            writer.bytes,
            [2, 0, 0, 0, b'a', b'b', 0, 0, 1, 0, 0, 0, b'c', 0, 1, b'b', 0, 0, 0, 0, 1, 0, 0, 0]
        );
    }

    #[test]
    fn dictionary_entries_align_to_eight_after_the_length() {
        let entry = Value::Entry(Box::new(Value::str("a")), Box::new(Value::variant(Value::Byte(7))));
        let bytes = written(&Value::Array(Ty::Entry(Box::new(Ty::Str), Box::new(Ty::Variant)), vec![entry]));
        // Length 10 (key 6 bytes, signature 3, byte 1), padding to 8, then the entry.
        assert_eq!(bytes, [10, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, b'a', 0, 1, b'y', 0, 7]);
    }

    #[test]
    fn empty_arrays_keep_their_padding_and_signature() {
        let empty = Value::Array(Ty::Struct(vec![Ty::Byte, Ty::Variant]), Vec::new());
        assert_eq!(written(&empty), [0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(empty.ty().signature(), "a(yv)");
    }

    #[test]
    fn signatures_round_trip() {
        for signature in ["a{oa{sa{sv}}}", "(ysa(ub))", "ouq", "as", "a{sv}", "v"] {
            let types = Ty::parse_all(signature).unwrap();
            assert_eq!(types.iter().map(Ty::signature).collect::<String>(), signature);
        }
        assert!(Ty::parse_all("a").is_err());
        assert!(Ty::parse_all("(").is_err());
        assert!(Ty::parse_all("{sv").is_err());
        assert!(Ty::parse_all("z").is_err());
        assert!(Ty::parse_all(&"a".repeat(64)).is_err());
    }

    fn sample_body() -> Vec<Value> {
        let properties = Value::Array(
            Ty::Entry(Box::new(Ty::Str), Box::new(Ty::Variant)),
            vec![
                Value::Entry(Box::new(Value::str("Powered")), Box::new(Value::variant(Value::Bool(true)))),
                Value::Entry(Box::new(Value::str("RSSI")), Box::new(Value::variant(Value::I16(-61)))),
                Value::Entry(Box::new(Value::str("Class")), Box::new(Value::variant(Value::U32(0x2508)))),
                Value::Entry(
                    Box::new(Value::str("UUIDs")),
                    Box::new(Value::variant(Value::Array(Ty::Str, vec![Value::str("00001124-0000-1000-8000-00805f9b34fb")]))),
                ),
            ],
        );
        vec![Value::path("/org/bluez/hci0/dev_00_11_22_33_44_55"), properties, Value::U64(1 << 40), Value::F64(0.5)]
    }

    #[test]
    fn calls_survive_encoding_and_decoding() {
        let call = MethodCall {
            destination: "org.bluez".into(),
            path: "/org/bluez/hci0".into(),
            interface: "org.freedesktop.DBus.Properties".into(),
            member: "Set".into(),
            body: sample_body(),
        };
        let bytes = encode_call(7, &call);
        assert_eq!(message_length(&bytes).unwrap(), Some(bytes.len()));
        let Message::Call(decoded) = decode(&bytes).unwrap() else { panic!("not a call") };
        assert_eq!((decoded.serial, decoded.path.as_str(), decoded.member.as_str()), (7, "/org/bluez/hci0", "Set"));
        assert_eq!(decoded.interface.as_deref(), Some("org.freedesktop.DBus.Properties"));
        assert_eq!(decoded.body, call.body);
        assert!(decoded.expects_reply);
    }

    #[test]
    fn replies_and_errors_carry_the_serial_they_answer() {
        let incoming = IncomingCall {
            serial: 41,
            sender: ":1.9".into(),
            path: "/agent".into(),
            interface: None,
            member: "RequestPinCode".into(),
            body: Vec::new(),
            expects_reply: true,
        };
        let bytes = encode_reply(3, &incoming, &Reply::Return(vec![Value::str("0000")]));
        assert_eq!(decode(&bytes).unwrap(), Message::Return { reply_serial: 41, body: vec![Value::str("0000")] });
        let bytes = encode_reply(4, &incoming, &Reply::Error { name: "org.bluez.Error.Rejected".into(), text: "no".into() });
        assert_eq!(
            decode(&bytes).unwrap(),
            Message::Error { reply_serial: 41, name: "org.bluez.Error.Rejected".into(), text: "no".into() }
        );
    }

    #[test]
    fn truncated_or_inconsistent_messages_are_errors() {
        let call = MethodCall { destination: "a.b".into(), path: "/".into(), interface: "a.b".into(), member: "M".into(), body: sample_body() };
        let bytes = encode_call(1, &call);
        for cut in [10, 20, bytes.len() - 1] {
            assert!(decode(&bytes[..cut]).is_err(), "decoded a message cut to {cut} bytes");
        }
        assert_eq!(message_length(&bytes[..10]).unwrap(), None);
        let mut big_endian = bytes.clone();
        big_endian[0] = b'B';
        assert!(message_length(&big_endian).is_err());
        let mut huge = bytes;
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(message_length(&huge).is_err());
    }

    /// Talks to a real `dbus-daemon`, which rejects malformed messages: run with
    /// `GHAZAL_TEST_DBUS_DAEMON=$(command -v dbus-daemon) cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a dbus-daemon binary"]
    fn dbus_daemon_accepts_our_messages_and_answers_in_kind() {
        struct Pin;
        impl Serve for Pin {
            fn call(&mut self, call: &IncomingCall) -> Reply {
                match call.member.as_str() {
                    "RequestPinCode" => Reply::Return(vec![Value::str("0000")]),
                    _ => Reply::Error { name: "org.bluez.Error.Rejected".into(), text: "refused".into() },
                }
            }
            fn aborted(&self) -> bool {
                false
            }
        }

        testing::with_daemon(|socket| {
            let mut server = Connection::open(socket).unwrap();
            let mut client = Connection::open(socket).unwrap();
            assert!(server.name().starts_with(':') && client.name() != server.name());
            let bus = |member: &str, body: Vec<Value>| MethodCall {
                destination: "org.freedesktop.DBus".into(),
                path: "/org/freedesktop/DBus".into(),
                interface: "org.freedesktop.DBus".into(),
                member: member.into(),
                body,
            };
            // Arrays of strings: the bus lists its own name and both connections.
            let names = client.call(&bus("ListNames", vec![]), Duration::from_secs(5), &mut Deaf).unwrap();
            let names: Vec<&str> = names[0].as_array().unwrap().iter().filter_map(Value::as_str).collect();
            assert!(names.contains(&"org.freedesktop.DBus") && names.contains(&server.name()), "{names:?}");
            // Dictionaries of variants.
            let credentials = client
                .call(&bus("GetConnectionCredentials", vec![Value::str("org.freedesktop.DBus")]), Duration::from_secs(5), &mut Deaf)
                .unwrap();
            assert!(credentials[0].entries().any(|(key, value)| key.as_str() == Some("UnixUserID") && value.as_u32().is_some()));
            // Errors from the bus.
            let missing = client.call(&bus("GetNameOwner", vec![Value::str("com.example.Nothing")]), Duration::from_secs(5), &mut Deaf);
            assert!(matches!(missing, Err(CallError::Remote { ref name, .. }) if name == "org.freedesktop.DBus.Error.NameHasNoOwner"));
            // Calls to an object we serve, answered while the server waits.
            let pin = MethodCall {
                destination: server.name().to_owned(),
                path: "/agent".into(),
                interface: "org.bluez.Agent1".into(),
                member: "RequestPinCode".into(),
                body: vec![Value::path("/org/bluez/hci0/dev_00_11_22_33_44_55")],
            };
            let serving = std::thread::spawn(move || {
                server.serve_for(Duration::from_millis(1500), &mut Pin).unwrap();
            });
            assert_eq!(client.call(&pin, Duration::from_secs(5), &mut Deaf).unwrap(), vec![Value::str("0000")]);
            let refused = client.call(&MethodCall { member: "Other".into(), ..pin }, Duration::from_secs(5), &mut Deaf);
            assert!(matches!(refused, Err(CallError::Remote { ref name, ref text }) if name == "org.bluez.Error.Rejected" && text == "refused"));
            serving.join().unwrap();
        });
    }
}

/// A private bus for tests that need a real daemon (`GHAZAL_TEST_DBUS_DAEMON`).
#[cfg(test)]
pub mod testing {
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    /// Runs `test` with the socket of a throwaway `dbus-daemon`, which is stopped afterwards.
    pub fn with_daemon(test: impl FnOnce(&Path) + std::panic::UnwindSafe) {
        let daemon = std::env::var("GHAZAL_TEST_DBUS_DAEMON").expect("GHAZAL_TEST_DBUS_DAEMON");
        let directory = std::env::temp_dir().join(format!("ghazal-dbus-test-{}-{:?}", std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("bus");
        let config = directory.join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                socket.display()
            ),
        )
        .unwrap();
        let mut child = Command::new(daemon).args(["--nofork", "--nopidfile", "--config-file"]).arg(&config).stdout(Stdio::null()).spawn().unwrap();
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let outcome = std::panic::catch_unwind(|| test(&socket));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&directory);
        outcome.unwrap();
    }
}
