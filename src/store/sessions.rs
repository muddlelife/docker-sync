use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub struct Session {
    pub token: String,
    pub csrf: String,
    pub authenticated: bool,
    pub expires: i64,
}
fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token))
}
impl Store {
    pub fn revoke_all_sessions(&self) -> Result<()> {
        self.conn()?.execute("DELETE FROM sessions", [])?;
        Ok(())
    }
    pub fn create_session(&self, authenticated: bool) -> Result<Session> {
        let mut bytes = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let session = Session {
            token: URL_SAFE_NO_PAD.encode(bytes),
            csrf: Uuid::new_v4().to_string(),
            authenticated,
            expires: now() + if authenticated { 43200 } else { 600 },
        };
        let conn = self.conn()?;
        conn.execute("DELETE FROM sessions WHERE expires<?1", [now()])?;
        conn.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                hash(&session.token),
                session.csrf,
                session.authenticated,
                session.expires
            ],
        )?;
        Ok(session)
    }
    pub fn session(&self, token: &str) -> Result<Option<Session>> {
        let value=self.conn()?.query_row("SELECT csrf,authenticated,expires FROM sessions WHERE token_hash=?1 AND expires>?2",rusqlite::params![hash(token),now()],|r|Ok(Session{token:token.into(),csrf:r.get(0)?,authenticated:r.get(1)?,expires:r.get(2)?})).optional()?;
        Ok(value)
    }
    pub fn delete_session(&self, token: &str) -> Result<()> {
        self.conn()?
            .execute("DELETE FROM sessions WHERE token_hash=?1", [hash(token)])?;
        Ok(())
    }
}
