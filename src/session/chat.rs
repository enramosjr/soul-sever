//! Room lines, private messages, tickers, and the censor table.
//!
//! Outgoing text is rewritten before it is framed. Incoming text is rewritten
//! before it becomes a [`ChatLine`](crate::model::ChatLine).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use crate::config::{Config, WordPair};
use crate::model::{ChatLine, RoomPerson};
use crate::protocol::chat::{
    decode_join_reply, decode_private, decode_private_room_name, decode_private_room_pair,
    decode_private_room_toggle, decode_private_room_users, decode_room_list, decode_say,
    decode_ticker_add, decode_ticker_remove, decode_ticker_state, decode_user_joined,
    decode_user_left, encode_message_acked, encode_message_user,
};
use crate::protocol::server;

use super::SessionEvent;

pub struct RoomChat {
    away: bool,
    auto_reply: String,
    censor: Vec<WordPair>,
    replace: Vec<WordPair>,
    log_rooms: bool,
    log_private: bool,
    room_log_dir: PathBuf,
    private_log_dir: PathBuf,
    members: HashMap<String, Vec<RoomPerson>>,
    tickers: HashMap<String, Vec<(String, String)>>,
}

pub enum Effect {
    Send(Vec<u8>),
    Event(SessionEvent),
}

impl RoomChat {
    pub fn new(config: &Config) -> Self {
        Self {
            away: false,
            auto_reply: config.auto_reply.clone(),
            censor: config.censor.clone(),
            replace: config.replace_words.clone(),
            log_rooms: config.log_rooms,
            log_private: config.log_private,
            room_log_dir: PathBuf::from(&config.room_log_dir),
            private_log_dir: PathBuf::from(&config.private_log_dir),
            members: HashMap::new(),
            tickers: HashMap::new(),
        }
    }

    pub fn set_away(&mut self, away: bool) {
        self.away = away;
    }

    pub fn apply_prefs(&mut self, prefs: &crate::settings::LivePrefs) {
        self.auto_reply.clone_from(&prefs.auto_reply);
        self.censor.clone_from(&prefs.censor);
        self.replace.clone_from(&prefs.replace_words);
        self.log_rooms = prefs.log_rooms;
        self.log_private = prefs.log_private;
        self.room_log_dir = PathBuf::from(&prefs.room_log_dir);
        self.private_log_dir = PathBuf::from(&prefs.private_log_dir);
    }

    pub fn outgoing(&self, text: &str) -> String {
        apply_words(text, &self.censor, &self.replace)
    }

    /// `None` means this code is not a chat message.
    pub async fn on_frame(
        &mut self,
        code: u32,
        payload: &[u8],
    ) -> Result<Option<Vec<Effect>>, crate::protocol::ProtocolError> {
        let mut effects = Vec::new();
        match code {
            server::JOIN_ROOM => {
                let reply = decode_join_reply(payload)?;
                let people = reply
                    .members
                    .iter()
                    .map(|member| RoomPerson {
                        name: member.name.clone(),
                        status: Some(member.status),
                        country: member.country.clone(),
                        files: Some(member.files),
                        dirs: Some(member.dirs),
                    })
                    .collect::<Vec<_>>();
                self.members.insert(reply.room.clone(), people.clone());
                effects.push(Effect::Event(SessionEvent::RoomMembers {
                    room: reply.room,
                    members: people,
                }));
            }
            server::SAY_CHATROOM => {
                let said = decode_say(payload)?;
                let text = self.outgoing(&said.message);
                let line = ChatLine {
                    room: said.room.clone(),
                    time: stamp(),
                    user: said.user.clone(),
                    text: text.clone(),
                };
                self.write_log(self.log_rooms, &self.room_log_dir, &said.room, &line)
                    .await;
                effects.push(Effect::Event(SessionEvent::Chat(line)));
            }
            server::USER_JOINED_ROOM => {
                let (room, user) = decode_user_joined(payload)?;
                let members = self.members.entry(room.clone()).or_default();
                if !members.iter().any(|person| person.name == user) {
                    members.push(RoomPerson::named(user));
                }
                effects.push(Effect::Event(SessionEvent::RoomMembers {
                    room,
                    members: members.clone(),
                }));
            }
            server::USER_LEFT_ROOM => {
                let (room, user) = decode_user_left(payload)?;
                if let Some(members) = self.members.get_mut(&room) {
                    members.retain(|person| person.name != user);
                }
                let members = self.members.get(&room).cloned().unwrap_or_default();
                effects.push(Effect::Event(SessionEvent::RoomMembers { room, members }));
            }
            server::MESSAGE_USER => {
                let message = decode_private(payload)?;
                let text = self.outgoing(&message.message);
                effects.push(Effect::Send(encode_message_acked(message.id)?));
                if self.away && !self.auto_reply.is_empty() {
                    let reply = self.outgoing(&self.auto_reply);
                    effects.push(Effect::Send(encode_message_user(&message.user, &reply)?));
                }
                let line = ChatLine {
                    room: message.user.clone(),
                    time: stamp(),
                    user: message.user.clone(),
                    text,
                };
                self.write_log(
                    self.log_private,
                    &self.private_log_dir,
                    &message.user,
                    &line,
                )
                .await;
                effects.push(Effect::Event(SessionEvent::RoomMembers {
                    room: message.user.clone(),
                    members: vec![RoomPerson::named(message.user.clone())],
                }));
                effects.push(Effect::Event(SessionEvent::Chat(line)));
            }
            server::ROOM_LIST => {
                let rooms = decode_room_list(payload)?;
                for (room, users) in rooms {
                    effects.push(Effect::Event(SessionEvent::RoomListed { room, users }));
                }
            }
            server::ROOM_TICKER_STATE => {
                let (room, lines) = decode_ticker_state(payload)?;
                self.tickers.insert(room.clone(), lines);
                effects.push(Effect::Event(SessionEvent::Ticker {
                    room: room.clone(),
                    text: ticker_text(self.tickers.get(&room)),
                }));
            }
            server::ROOM_TICKER_ADD => {
                let (room, user, text) = decode_ticker_add(payload)?;
                let lines = self.tickers.entry(room.clone()).or_default();
                if let Some(found) = lines.iter_mut().find(|(name, _)| name == &user) {
                    found.1 = text;
                } else {
                    lines.push((user, text));
                }
                effects.push(Effect::Event(SessionEvent::Ticker {
                    room: room.clone(),
                    text: ticker_text(self.tickers.get(&room)),
                }));
            }
            server::ROOM_TICKER_REMOVE => {
                let (room, user) = decode_ticker_remove(payload)?;
                if let Some(lines) = self.tickers.get_mut(&room) {
                    lines.retain(|(name, _)| name != &user);
                }
                effects.push(Effect::Event(SessionEvent::Ticker {
                    room: room.clone(),
                    text: ticker_text(self.tickers.get(&room)),
                }));
            }
            server::PRIVATE_ROOM_USERS | server::PRIVATE_ROOM_OPERATORS => {
                let (room, users) = decode_private_room_users(payload)?;
                let people = users.into_iter().map(RoomPerson::named).collect::<Vec<_>>();
                self.members.insert(room.clone(), people.clone());
                effects.push(Effect::Event(SessionEvent::RoomMembers {
                    room,
                    members: people,
                }));
            }
            server::PRIVATE_ROOM_ADD_USER
            | server::PRIVATE_ROOM_REMOVE_USER
            | server::PRIVATE_ROOM_ADD_OPERATOR
            | server::PRIVATE_ROOM_REMOVE_OPERATOR => {
                let _pair = decode_private_room_pair(payload)?;
            }
            server::PRIVATE_ROOM_CANCEL_MEMBERSHIP
            | server::PRIVATE_ROOM_DISOWN
            | server::PRIVATE_ROOM_SOMETHING
            | server::PRIVATE_ROOM_ADDED
            | server::PRIVATE_ROOM_REMOVED
            | server::PRIVATE_ROOM_OPERATOR_ADDED
            | server::PRIVATE_ROOM_OPERATOR_REMOVED => {
                let _room = decode_private_room_name(payload)?;
            }
            server::PRIVATE_ROOM_TOGGLE => {
                let _enabled = decode_private_room_toggle(payload)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(effects))
    }

    async fn write_log(&self, enabled: bool, dir: &PathBuf, name: &str, line: &ChatLine) {
        if !enabled || dir.as_os_str().is_empty() {
            return;
        }
        if tokio::fs::create_dir_all(dir).await.is_err() {
            return;
        }
        let path = dir.join(log_file(name));
        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            Ok(file) => file,
            Err(_) => return,
        };
        let row = format!("{} {} {}\n", line.time, line.user, line.text);
        let _ = file.write_all(row.as_bytes()).await;
    }
}

pub fn apply_words(text: &str, censor: &[WordPair], replace: &[WordPair]) -> String {
    let mut out = text.to_owned();
    for pair in censor.iter().chain(replace) {
        if !pair.from.is_empty() {
            out = out.replace(&pair.from, &pair.to);
        }
    }
    out
}

fn ticker_text(lines: Option<&Vec<(String, String)>>) -> String {
    lines
        .map(|lines| {
            lines
                .iter()
                .map(|(user, text)| format!("{user}: {text}"))
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .unwrap_or_default()
}

fn log_file(name: &str) -> String {
    let mut file = name.replace(['/', '\\'], "_");
    file.push_str(".log");
    file
}

fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let minutes = (secs / 60) % (24 * 60);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_censored_token_is_replaced() {
        let censor = vec![WordPair {
            from: "secret".to_owned(),
            to: "****".to_owned(),
        }];
        assert_eq!(apply_words("hello secret", &censor, &[]), "hello ****");
    }
}
