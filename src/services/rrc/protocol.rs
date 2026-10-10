//! RRC v1 CBOR envelopes and session semantics.
//! https://rrc.kc1awv.net/3-RRC-wire-encoding.html
use anyhow::{Context, Result, bail, ensure};
use ciborium::Value;
use rand::RngCore;
use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    Hello,
    Welcome,
    Join,
    Joined,
    Part,
    Parted,
    Message,
    Notice,
    Action,
    Ping,
    Pong,
    Error,
    Unknown(u64),
}
impl MessageType {
    fn number(self) -> u64 {
        match self {
            Self::Hello => 1,
            Self::Welcome => 2,
            Self::Join => 10,
            Self::Joined => 11,
            Self::Part => 12,
            Self::Parted => 13,
            Self::Message => 20,
            Self::Notice => 21,
            Self::Action => 22,
            Self::Ping => 30,
            Self::Pong => 31,
            Self::Error => 40,
            Self::Unknown(n) => n,
        }
    }
    fn from_number(n: u64) -> Self {
        match n {
            1 => Self::Hello,
            2 => Self::Welcome,
            10 => Self::Join,
            11 => Self::Joined,
            12 => Self::Part,
            13 => Self::Parted,
            20 => Self::Message,
            21 => Self::Notice,
            22 => Self::Action,
            30 => Self::Ping,
            31 => Self::Pong,
            40 => Self::Error,
            n => Self::Unknown(n),
        }
    }
}

pub struct Envelope {
    pub kind: MessageType,
    pub id: [u8; 8],
    pub timestamp: u64,
    pub source: [u8; 16],
    pub room: Option<String>,
    pub body: Option<Value>,
    pub nickname: Option<String>,
}

fn uint(value: u64) -> Value {
    Value::Integer(value.into())
}
fn number(value: &Value) -> Result<u64> {
    match value {
        Value::Integer(n) => u64::try_from(*n).context("RRC integer must be unsigned"),
        _ => bail!("RRC field must be an unsigned integer"),
    }
}
fn bytes<const N: usize>(value: &Value) -> Result<[u8; N]> {
    match value {
        Value::Bytes(b) => b
            .as_slice()
            .try_into()
            .context("invalid RRC byte-string length"),
        _ => bail!("RRC field must be a byte string"),
    }
}
fn optional_text(value: Option<&Value>) -> Result<Option<String>> {
    match value {
        Some(Value::Text(s)) => Ok(Some(s.clone())),
        None => Ok(None),
        _ => bail!("RRC field must be text"),
    }
}

impl Envelope {
    pub fn new(
        kind: MessageType,
        source: [u8; 16],
        nickname: &str,
        room: Option<String>,
        body: Option<Value>,
    ) -> Self {
        let mut id = [0; 8];
        rand::rngs::OsRng.fill_bytes(&mut id);
        Self {
            kind,
            id,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            source,
            room,
            body,
            nickname: Some(nickname.into()),
        }
    }
    pub fn hello(source: [u8; 16], nickname: &str) -> Self {
        Self::new(
            MessageType::Hello,
            source,
            nickname,
            None,
            Some(Value::Map(vec![
                (uint(0), Value::Text("bridge-bot".into())),
                (uint(1), Value::Text(env!("CARGO_PKG_VERSION").into())),
            ])),
        )
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut fields = vec![
            (uint(0), uint(1)),
            (uint(1), uint(self.kind.number())),
            (uint(2), Value::Bytes(self.id.to_vec())),
            (uint(3), uint(self.timestamp)),
            (uint(4), Value::Bytes(self.source.to_vec())),
        ];
        if let Some(room) = &self.room {
            fields.push((uint(5), Value::Text(room.clone())));
        }
        if let Some(body) = &self.body {
            fields.push((uint(6), body.clone()));
        }
        if let Some(nickname) = &self.nickname {
            fields.push((uint(7), Value::Text(nickname.clone())));
        }
        let mut encoded = Vec::new();
        ciborium::into_writer(&Value::Map(fields), &mut encoded)?;
        Ok(encoded)
    }
    pub fn decode(encoded: &[u8]) -> Result<Self> {
        ensure!(encoded.len() <= 65536, "RRC envelope too large");
        let mut input = encoded;
        let value: Value = ciborium::from_reader(&mut input).context("invalid RRC CBOR")?;
        ensure!(input.is_empty(), "trailing data after RRC envelope");
        let Value::Map(entries) = value else {
            bail!("RRC envelope must be a map");
        };
        let mut fields = HashMap::new();
        for (key, value) in entries {
            ensure!(
                fields.insert(number(&key)?, value).is_none(),
                "duplicate RRC envelope key"
            );
        }
        let required = |key| {
            fields
                .get(&key)
                .context("required RRC envelope field missing")
        };
        ensure!(number(required(0)?)? == 1, "unsupported RRC version");
        Ok(Self {
            kind: MessageType::from_number(number(required(1)?)?),
            id: bytes(required(2)?)?,
            timestamp: number(required(3)?)?,
            source: bytes(required(4)?)?,
            room: optional_text(fields.get(&5))?,
            body: fields.get(&6).cloned(),
            nickname: optional_text(fields.get(&7))?,
        })
    }
    pub fn text(&self) -> Option<&str> {
        match &self.body {
            Some(Value::Text(text)) => Some(text),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct Outcome {
    pub join: Vec<String>,
    pub pong: bool,
    pub chat: bool,
}

pub struct SessionState {
    hub: [u8; 16],
    local: [u8; 16],
    rooms: HashSet<String>,
    welcomed: bool,
    awaiting: HashSet<String>,
    pub joined: HashSet<String>,
}

impl SessionState {
    pub fn new(hub: [u8; 16], local: [u8; 16], rooms: HashSet<String>) -> Self {
        Self {
            hub,
            local,
            rooms,
            welcomed: false,
            awaiting: HashSet::new(),
            joined: HashSet::new(),
        }
    }
    pub fn is_ready(&self) -> bool {
        self.welcomed && self.rooms == self.joined
    }
    pub fn is_hub(&self, source: [u8; 16]) -> bool {
        source == self.hub
    }
    pub fn apply(&mut self, message: &Envelope) -> Result<Outcome> {
        let mut outcome = Outcome::default();
        let room = message.room.as_ref().map(|s| s.to_lowercase());
        match message.kind {
            MessageType::Welcome if message.source == self.hub && !self.welcomed => {
                self.welcomed = true;
                self.awaiting.clone_from(&self.rooms);
                outcome.join = self.rooms.iter().cloned().collect();
            }
            MessageType::Joined if message.source == self.hub && self.welcomed => {
                if let Some(room) = room
                    && self.awaiting.remove(&room)
                {
                    self.joined.insert(room);
                }
            }
            MessageType::Parted if message.source == self.hub => {
                let ours = match &message.body {
                    Some(Value::Bytes(hash)) => hash.as_slice() == self.local,
                    Some(Value::Array(members)) => members.iter().any(|member| matches!(member, Value::Bytes(hash) if hash.as_slice() == self.local)),
                    None => message.nickname.is_none(),
                    _ => false,
                };
                if ours
                    && let Some(room) = room
                    && self.joined.remove(&room)
                {
                    bail!("RRC hub removed the bot from room {room}");
                }
            }
            MessageType::Ping => outcome.pong = true,
            MessageType::Error if message.source == self.hub => {
                return Err(super::SendFailure::Permanent("RRC hub rejected the session").into());
            }
            MessageType::Message | MessageType::Action
                if message.source != self.local && message.text().is_some() =>
            {
                outcome.chat = room.as_ref().is_some_and(|r| self.joined.contains(r));
            }
            _ => {}
        }
        Ok(outcome)
    }
}

pub fn text_packets(
    source: [u8; 16],
    nickname: &str,
    room: &str,
    mut text: &str,
    mdu: usize,
) -> Result<Vec<Vec<u8>>> {
    // Reserve space for a hub to replace our nickname by up to 32 UTF-8 bytes.
    let budget = mdu
        .min(350)
        .checked_sub(32usize.saturating_sub(nickname.len()))
        .context("RRC MDU too small")?;
    let mut packets = Vec::new();
    while !text.is_empty() {
        let mut message = Envelope::new(
            MessageType::Message,
            source,
            nickname,
            Some(room.into()),
            None,
        );
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect();
        let (mut low, mut high) = (0, boundaries.len() - 1);
        while low < high {
            let mid = (low + high).div_ceil(2);
            message.body = Some(Value::Text(text[..boundaries[mid]].into()));
            if message.encode()?.len() <= budget {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        ensure!(low > 0, "RRC MDU leaves no room for text");
        let end = boundaries[low];
        message.body = Some(Value::Text(text[..end].into()));
        packets.push(message.encode()?);
        text = &text[end..];
    }
    Ok(packets)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(
        kind: MessageType,
        source: [u8; 16],
        room: Option<&str>,
        text: Option<&str>,
    ) -> Envelope {
        Envelope::new(
            kind,
            source,
            "nick",
            room.map(str::to_owned),
            text.map(|s| Value::Text(s.into())),
        )
    }
    #[test]
    fn unicode_chunking_and_wire_roundtrip() {
        let text = "Hallo 🌍 äöü! ".repeat(90);
        let packets = text_packets([1; 16], "bridge-bot", "#lobby", &text, 431).unwrap();
        assert!(packets.len() > 1);
        let mut ids = HashSet::new();
        let mut joined = String::new();
        for packet in packets {
            assert!(packet.len() <= 350 - 22);
            let message = Envelope::decode(&packet).unwrap();
            assert_eq!(message.kind, MessageType::Message);
            assert_eq!(message.source, [1; 16]);
            assert_eq!(message.room.as_deref(), Some("#lobby"));
            assert!(ids.insert(message.id));
            joined.push_str(message.text().unwrap());
        }
        assert_eq!(joined, text);
        assert!(text_packets([1; 16], "nick", "#lobby", "🌍", 10).is_err());
    }
    #[test]
    fn session_requires_welcome_and_joined_filters_echoes_and_notices() {
        let mut state = SessionState::new([9; 16], [1; 16], HashSet::from(["#lobby".into()]));
        state
            .apply(&event(MessageType::Joined, [9; 16], Some("#lobby"), None))
            .unwrap();
        assert!(!state.is_ready());
        assert!(
            state
                .apply(&event(MessageType::Welcome, [2; 16], None, None))
                .unwrap()
                .join
                .is_empty()
        );
        assert_eq!(
            state
                .apply(&event(MessageType::Welcome, [9; 16], None, None))
                .unwrap()
                .join,
            vec!["#lobby"]
        );
        state
            .apply(&event(MessageType::Joined, [9; 16], Some("#LOBBY"), None))
            .unwrap();
        assert!(state.is_ready());
        assert!(
            state
                .apply(&event(
                    MessageType::Message,
                    [2; 16],
                    Some("#lobby"),
                    Some("hello")
                ))
                .unwrap()
                .chat
        );
        assert!(
            !state
                .apply(&event(
                    MessageType::Message,
                    [1; 16],
                    Some("#lobby"),
                    Some("echo")
                ))
                .unwrap()
                .chat
        );
        assert!(
            !state
                .apply(&event(
                    MessageType::Message,
                    [2; 16],
                    Some("#elsewhere"),
                    Some("hello")
                ))
                .unwrap()
                .chat
        );
        assert!(
            !state
                .apply(&event(
                    MessageType::Notice,
                    [9; 16],
                    Some("#lobby"),
                    Some("system")
                ))
                .unwrap()
                .chat
        );
        state
            .apply(&event(MessageType::Parted, [9; 16], Some("#lobby"), None))
            .unwrap();
        assert!(
            state.is_ready(),
            "another member leaving must not remove our membership"
        );
        let mut own_part = event(MessageType::Parted, [9; 16], Some("#lobby"), None);
        own_part.body = Some(Value::Bytes(vec![1; 16]));
        assert!(state.apply(&own_part).is_err());
    }
    #[test]
    fn ping_preserves_arbitrary_body_and_extensions_are_ignored() {
        let mut state = SessionState::new([9; 16], [1; 16], HashSet::new());
        let mut ping = event(MessageType::Ping, [9; 16], None, None);
        ping.body = Some(Value::Array(vec![uint(4), Value::Bytes(vec![0, 255])]));
        let ping = Envelope::decode(&ping.encode().unwrap()).unwrap();
        assert!(state.apply(&ping).unwrap().pong);
        let pong = Envelope::new(MessageType::Pong, [1; 16], "bot", None, ping.body.clone());
        assert_eq!(
            Envelope::decode(&pong.encode().unwrap()).unwrap().body,
            ping.body
        );
        let unknown = event(MessageType::Unknown(99), [9; 16], None, None);
        assert!(!state.apply(&unknown).unwrap().chat);
    }
    #[test]
    fn rejects_malformed_envelopes_but_accepts_unknown_keys() {
        assert!(Envelope::decode(&[0xff]).is_err());
        let good = event(MessageType::Message, [2; 16], Some("#lobby"), Some("hello"))
            .encode()
            .unwrap();
        let mut value: Value = ciborium::from_reader(good.as_slice()).unwrap();
        if let Value::Map(fields) = &mut value {
            fields.push((uint(50), Value::Text("future".into())));
        }
        let mut encoded = Vec::new();
        ciborium::into_writer(&value, &mut encoded).unwrap();
        assert!(Envelope::decode(&encoded).is_ok());
        if let Value::Map(fields) = &mut value {
            fields.push((uint(0), uint(1)));
        }
        encoded.clear();
        ciborium::into_writer(&value, &mut encoded).unwrap();
        assert!(Envelope::decode(&encoded).is_err());
        let mut trailing = good;
        trailing.push(0);
        assert!(Envelope::decode(&trailing).is_err());
    }
}
