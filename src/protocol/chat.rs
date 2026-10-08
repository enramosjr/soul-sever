//! Chat, ticker, and private-room messages. Layouts match Nicotine+ 3.3.11.
//!
//! `SayChatroom`, `JoinRoom`, and `MessageUser` use a shorter body when the client
//! sends them than when the server delivers them.

use super::codes::server;
use super::wire::{ProtocolError, Reader, Writer, encode_frame};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomMember {
    pub name: String,
    pub status: u32,
    pub country: String,
    pub files: u32,
    pub dirs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinReply {
    pub room: String,
    pub members: Vec<RoomMember>,
    pub owner: Option<String>,
    pub operators: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingSay {
    pub room: String,
    pub user: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingPrivate {
    pub id: u32,
    pub timestamp: u32,
    pub user: String,
    pub message: String,
    pub is_new: bool,
}

pub fn encode_join_room(room: &str, private: bool) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(room)?;
    writer.u32(u32::from(private));
    encode_frame(server::JOIN_ROOM, &writer.finish())
}

pub fn encode_leave_room(room: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(room)?;
    encode_frame(server::LEAVE_ROOM, &writer.finish())
}

pub fn encode_say_chatroom(room: &str, message: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(room)?;
    writer.string(message)?;
    encode_frame(server::SAY_CHATROOM, &writer.finish())
}

pub fn encode_message_user(user: &str, message: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(user)?;
    writer.string(message)?;
    encode_frame(server::MESSAGE_USER, &writer.finish())
}

pub fn encode_message_acked(id: u32) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.u32(id);
    encode_frame(server::MESSAGE_ACKED, &writer.finish())
}

pub fn encode_room_list_request() -> Result<Vec<u8>, ProtocolError> {
    encode_frame(server::ROOM_LIST, &[])
}

pub fn encode_room_ticker_set(room: &str, message: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(room)?;
    writer.string(message)?;
    encode_frame(server::ROOM_TICKER_SET, &writer.finish())
}

pub fn encode_private_room_pair(
    code: u32,
    room: &str,
    user: &str,
) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(room)?;
    writer.string(user)?;
    encode_frame(code, &writer.finish())
}

pub fn encode_private_room_name(code: u32, room: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(room)?;
    encode_frame(code, &writer.finish())
}

pub fn encode_private_room_toggle(enabled: bool) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.u8(u8::from(enabled));
    encode_frame(server::PRIVATE_ROOM_TOGGLE, &writer.finish())
}

pub fn decode_join_reply(payload: &[u8]) -> Result<JoinReply, ProtocolError> {
    let mut reader = Reader::new(payload);
    let room = reader.string()?;
    let members = read_members(&mut reader)?;
    let owner = if reader.rest().is_empty() {
        None
    } else {
        Some(reader.string()?)
    };
    let mut operators = Vec::new();
    if owner.is_some() && !reader.rest().is_empty() {
        let count = reader.u32()?;
        for _ in 0..count {
            operators.push(reader.string()?);
        }
    }
    Ok(JoinReply {
        room,
        members,
        owner,
        operators,
    })
}

fn read_members(reader: &mut Reader<'_>) -> Result<Vec<RoomMember>, ProtocolError> {
    let count = reader.u32()?;
    let mut names = Vec::new();
    for _ in 0..count {
        names.push(reader.string()?);
    }
    let status_count = reader.u32()?;
    let mut status = Vec::new();
    for _ in 0..status_count {
        status.push(reader.u32()?);
    }
    let stat_count = reader.u32()?;
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for _ in 0..stat_count {
        let _average = reader.u32()?;
        let _uploads = reader.u32()?;
        let _unknown = reader.u32()?;
        files.push(reader.u32()?);
        dirs.push(reader.u32()?);
    }
    let slot_count = reader.u32()?;
    for _ in 0..slot_count {
        let _slots = reader.u32()?;
    }
    let country_count = reader.u32()?;
    let mut countries = Vec::new();
    for _ in 0..country_count {
        countries.push(reader.string()?);
    }
    let mut members = Vec::new();
    for (index, name) in names.into_iter().enumerate() {
        members.push(RoomMember {
            name,
            status: status.get(index).copied().unwrap_or(0),
            country: countries.get(index).cloned().unwrap_or_default(),
            files: files.get(index).copied().unwrap_or(0),
            dirs: dirs.get(index).copied().unwrap_or(0),
        });
    }
    Ok(members)
}

pub fn decode_say(payload: &[u8]) -> Result<IncomingSay, ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(IncomingSay {
        room: reader.string()?,
        user: reader.string()?,
        message: reader.string()?,
    })
}

pub fn decode_user_joined(payload: &[u8]) -> Result<(String, String), ProtocolError> {
    let mut reader = Reader::new(payload);
    let room = reader.string()?;
    let user = reader.string()?;
    let _status = reader.u32()?;
    let _average = reader.u32()?;
    let _uploads = reader.u32()?;
    let _unknown = reader.u32()?;
    let _files = reader.u32()?;
    let _dirs = reader.u32()?;
    let _slots = reader.u32()?;
    let _country = reader.string()?;
    Ok((room, user))
}

pub fn decode_user_left(payload: &[u8]) -> Result<(String, String), ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok((reader.string()?, reader.string()?))
}

pub fn decode_private(payload: &[u8]) -> Result<IncomingPrivate, ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(IncomingPrivate {
        id: reader.u32()?,
        timestamp: reader.u32()?,
        user: reader.string()?,
        message: reader.string()?,
        is_new: reader.bool()?,
    })
}

pub fn decode_room_list(payload: &[u8]) -> Result<Vec<(String, u32)>, ProtocolError> {
    let mut reader = Reader::new(payload);
    let public = read_room_counts(&mut reader)?;
    if !reader.rest().is_empty() {
        let _owned = read_room_counts(&mut reader)?;
    }
    if !reader.rest().is_empty() {
        let _other = read_room_counts(&mut reader)?;
    }
    if !reader.rest().is_empty() {
        let _operated = read_names(&mut reader)?;
    }
    Ok(public)
}

fn read_room_counts(reader: &mut Reader<'_>) -> Result<Vec<(String, u32)>, ProtocolError> {
    let names = read_names(reader)?;
    let count = reader.u32()?;
    let mut rooms = Vec::new();
    for (index, name) in names.into_iter().enumerate() {
        let users = if u32::try_from(index).unwrap_or(u32::MAX) < count {
            reader.u32()?
        } else {
            0
        };
        rooms.push((name, users));
    }
    Ok(rooms)
}

fn read_names(reader: &mut Reader<'_>) -> Result<Vec<String>, ProtocolError> {
    let count = reader.u32()?;
    let mut names = Vec::new();
    for _ in 0..count {
        names.push(reader.string()?);
    }
    Ok(names)
}

pub fn decode_ticker_state(
    payload: &[u8],
) -> Result<(String, Vec<(String, String)>), ProtocolError> {
    let mut reader = Reader::new(payload);
    let room = reader.string()?;
    let count = reader.u32()?;
    let mut lines = Vec::new();
    for _ in 0..count {
        lines.push((reader.string()?, reader.string()?));
    }
    Ok((room, lines))
}

pub fn decode_ticker_add(payload: &[u8]) -> Result<(String, String, String), ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok((reader.string()?, reader.string()?, reader.string()?))
}

pub fn decode_ticker_remove(payload: &[u8]) -> Result<(String, String), ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok((reader.string()?, reader.string()?))
}

pub fn decode_private_room_users(payload: &[u8]) -> Result<(String, Vec<String>), ProtocolError> {
    let mut reader = Reader::new(payload);
    let room = reader.string()?;
    Ok((room, read_names(&mut reader)?))
}

pub fn decode_private_room_pair(payload: &[u8]) -> Result<(String, String), ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok((reader.string()?, reader.string()?))
}

pub fn decode_private_room_name(payload: &[u8]) -> Result<String, ProtocolError> {
    Reader::new(payload).string()
}

pub fn decode_private_room_toggle(payload: &[u8]) -> Result<bool, ProtocolError> {
    Reader::new(payload).bool()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_messages_match_the_nicotine_layout() {
        assert_eq!(
            encode_join_room("jazz", false).unwrap(),
            hex("100000000e000000040000006a617a7a00000000")
        );
        assert_eq!(
            encode_say_chatroom("jazz", "hi").unwrap(),
            hex("120000000d000000040000006a617a7a020000006869")
        );
        let said = decode_say(&hex("040000006a617a7a03000000626f62020000006869")).unwrap();
        assert_eq!(said.user, "bob");
        assert_eq!(said.message, "hi");
        assert_eq!(
            encode_message_user("bob", "hi").unwrap(),
            hex("110000001600000003000000626f62020000006869")
        );
        assert_eq!(
            encode_message_acked(7).unwrap(),
            hex("080000001700000007000000")
        );
        let private = decode_private(&hex("070000000100000003000000626f6202000000686901")).unwrap();
        assert_eq!(private.id, 7);
        assert_eq!(private.user, "bob");
        assert!(private.is_new);
        assert_eq!(
            encode_private_room_pair(server::PRIVATE_ROOM_ADD_USER, "jazz", "bob").unwrap(),
            hex("1300000086000000040000006a617a7a03000000626f62")
        );
        assert_eq!(
            encode_private_room_toggle(true).unwrap(),
            hex("050000008d00000001")
        );
        let (room, users) =
            decode_private_room_users(&hex("040000006a617a7a0100000003000000626f62")).unwrap();
        assert_eq!(room, "jazz");
        assert_eq!(users, vec!["bob".to_owned()]);
        let members = decode_join_reply(&hex(
            "040000006a617a7a0100000003000000626f620100000002000000010000000000000000000000000000000000000000000000010000000000000001000000020000005553",
        ))
        .unwrap();
        assert_eq!(members.room, "jazz");
        assert_eq!(members.members[0].name, "bob");
        assert_eq!(members.members[0].status, 2);
        assert_eq!(members.members[0].country, "US");
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }
}
