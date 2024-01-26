use std::collections::HashMap;
use std::sync::Arc;

use color_eyre::{
    eyre::{eyre, WrapErr},
    Result,
};
use config::{Config, File as CFile, FileFormat as CFFormat};
use sqlx::{sqlite, Row};
use tracing::{debug, error, info, trace};

use serenity::all::{ChannelId, GuildId, Member};
use serenity::async_trait;
use serenity::model::{channel::Message, user::User};
use serenity::prelude::*;

const COMMAND_PREFIX: &str = "!abs ";

#[derive(Debug)]
struct Handler {
    db_pool: sqlite::SqlitePool,
}

impl Handler {
    async fn get_notify_channel(&self, guild_id: &GuildId) -> Result<ChannelId> {
        let query = sqlx::query("SELECT channel_id FROM notify_channel WHERE guild_id = ?")
            .bind(format!("{}", guild_id.get()))
            .fetch_one(&self.db_pool)
            .await
            .wrap_err("failed to get notify channel from db")?;
        let cid_raw = query
            .get::<&str, _>("channel_id")
            .parse()
            .expect("malformed channel id integer returned from database");
        Ok(ChannelId::new(cid_raw))
    }

    async fn set_notify_channel(&self, guild_id: &GuildId, channel_id: &ChannelId) -> Result<()> {
        // TODO: transaction??
        sqlx::query("DELETE FROM notify_channel WHERE guild_id = ?")
            .bind(format!("{}", guild_id.get()))
            .execute(&self.db_pool)
            .await
            .wrap_err("failed to clear old notify channel")?;
        sqlx::query("INSERT INTO notify_channel (guild_id, channel_id) VALUES(?, ?)")
            .bind(format!("{}", guild_id.get()))
            .bind(format!("{}", channel_id.get()))
            .execute(&self.db_pool)
            .await
            .wrap_err("failed to insert notify channel")?;

        Ok(())
    }

    fn parse_set_channel(content: &str) -> Result<Option<ChannelId>> {
        let Some(content) = content.strip_prefix(COMMAND_PREFIX) else {
            trace!("non-command message: {}", content);
            return Ok(None);
        };

        let Some(cid_lit) = content.strip_prefix("notifchan ") else {
            return Err(eyre!(
                "bad command format, use: `{COMMAND_PREFIX} notifchan <channelid>`"
            ));
        };

        const CID_INVALID: &str = "channel id invalid";
        if cid_lit == "0" {
            return Err(eyre!(CID_INVALID));
        }
        let cid_lit: u64 = cid_lit.parse().map_err(|_| eyre!(CID_INVALID))?;
        Ok(Some(ChannelId::new(cid_lit)))
    }
}

macro_rules! log_err_and_return {
    ($err_str:expr) => {{
        error!($err_str);
        return;
    }};
}

macro_rules! reply_and_return {
    ($orig_msg:expr, $content:expr, $ctx:expr) => {{
        match $orig_msg.reply_mention(&$ctx, $content).await {
            Ok(_) => (),
            Err(e) => log_err_and_return!("couldn't reply to message: {e}"),
        }
        return;
    }};
}

#[async_trait]
impl EventHandler for Handler {
    #[tracing::instrument]
    async fn guild_member_removal(
        &self,
        ctx: Context,
        guild_id: GuildId,
        user: User,
        _member_data: Option<Member>,
    ) {
        debug!("guild member removed");

        let notify_cid = match self.get_notify_channel(&guild_id).await {
            Ok(i) => i,
            Err(e) => log_err_and_return!("{e}"),
        };

        let guild_channels = match guild_id.channels(&ctx.http).await {
            Ok(c) => c,
            Err(e) => log_err_and_return!("error getting channels for {guild_id}: {e}"),
        };

        let to_notif = guild_channels
            .get(&notify_cid)
            .ok_or_else(|| eyre!("guild {guild_id} doesn't have a channel {notify_cid}"))
            .unwrap();

        let content = format!("{} ({}) has left the server", user.name, user.id);
        match to_notif.say(ctx, content).await {
            Ok(_) => debug!("leaving message sent to {guild_id}"),
            Err(e) => log_err_and_return!(
                "couldn't send message to channel {notify_cid} in guild {guild_id}: {e}"
            ),
        }
    }

    #[tracing::instrument]
    async fn message(&self, ctx: Context, new_message: Message) {
        let cid = match Self::parse_set_channel(&new_message.content) {
            Ok(Some(cid)) => cid,
            Ok(None) => return,
            Err(e) => match new_message.reply_mention(&ctx, e).await {
                Ok(_) => return,
                Err(e) => log_err_and_return!("{e}"),
            },
        };

        match cid
            .say(
                &ctx,
                "This is now the channel that will be notified when someone leaves.",
            )
            .await
        {
            Ok(_) => (),
            Err(e) => {
                error!("couldn't send message to channel {cid}: {e}");
                reply_and_return!(
                    new_message,
                    "I can't find or don't have access to that channel",
                    ctx
                );
            }
        }

        let gid = new_message
            .guild_id
            .expect("no guild id attached to message");

        match self.set_notify_channel(&gid, &cid).await {
            Ok(_) => (),
            Err(e) => log_err_and_return!("{e}"),
        }
    }
}

#[derive(serde::Deserialize)]
struct Options {
    discord_token: String,
    db_path: String,
}

impl Options {
    fn get() -> Result<Self> {
        Ok(Config::builder()
            .add_source(config::Environment::default())
            .add_source(CFile::new("./config.toml", CFFormat::Toml).required(false))
            .set_default("db_path", "./channels.db")?
            .build()?
            .try_deserialize()?)
    }
}

#[tracing::instrument]
async fn db_init(filename: &str) -> Result<sqlite::SqlitePool> {
    let options = sqlite::SqliteConnectOptions::new()
        .create_if_missing(true)
        .filename(filename);

    debug!("attempting to open database connection to '{}'", filename);
    let db = sqlite::SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .wrap_err("failed connecting to sqlite database")?;

    info!("running database migrations");
    sqlx::migrate!()
        .run(
            &mut db
                .acquire()
                .await
                .wrap_err("failed to acquire db connection from pool")?,
        )
        .await
        .wrap_err("failed running database migrations")?;

    Ok(db)
}

#[tokio::main]
#[tracing::instrument]
async fn main() -> Result<()> {
    color_eyre::install().expect("couldn't initialise eyre");
    tracing_subscriber::fmt::init();

    let options = Options::get().wrap_err("failed to get configuration")?;
    let db_pool = db_init(&options.db_path).await?;
    let intents = GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let handler = Handler {
        db_pool,
    };

    info!("starting client");
    let mut client = Client::builder(&options.discord_token, intents)
        .event_handler(handler)
        .await
        .wrap_err("error creating client")?;

    client.start().await.wrap_err("client start error")?;
    unreachable!("client exited")
}
