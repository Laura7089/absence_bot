use std::collections::HashMap;
use std::sync::Arc;

use tracing::{error, trace, debug, info};
use color_eyre::{eyre::{eyre, WrapErr}, Result};

use serenity::all::ChannelId;
use serenity::all::GuildId;
use serenity::all::Member;
use serenity::async_trait;
use serenity::model::channel::Message;
use serenity::model::user::User;
use serenity::prelude::*;

const COMMAND_PREFIX: &str = "!abs ";

#[derive(Debug)]
struct Handler {
    notify_channels: Arc<Mutex<HashMap<GuildId, ChannelId>>>,
}

macro_rules! log_err_and_return {
    ($err_str:expr) => {
        {
            error!($err_str);
            return;
        }
    };
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
        let notify_cid = {
            let notify_channels = self.notify_channels.lock().await;
            let Some(&notify_cid) = notify_channels.get(&guild_id) else {
                log_err_and_return!("no notify channel set for {guild_id}");
            };
            notify_cid
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
            Err(e) => log_err_and_return!("couldn't send message to channel {notify_cid} in guild {guild_id}: {e}"),
        }
    }

    #[tracing::instrument]
    async fn message(&self, ctx: Context, new_message: Message) {
        let Some(content) = new_message.content.strip_prefix(COMMAND_PREFIX) else {
            trace!("non-command message: {}", new_message.content);
            return;
        };

        let Some(cid_lit) = content.strip_prefix("notifchan ") else {
            let reply = format!("bad command format, use: `{COMMAND_PREFIX} notifchan <channelid>`");
            reply_and_return!(new_message, reply, ctx);
        };

        const CID_INVALID: &str = "channel id invalid";
        if cid_lit == "0" {
            reply_and_return!(new_message, CID_INVALID, ctx);
        }
        let cid_lit: u64 = match cid_lit.parse() {
            Ok(x) => x,
            Err(_) => reply_and_return!(new_message, CID_INVALID, ctx),
        };
        let cid = ChannelId::new(cid_lit);

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
                reply_and_return!(new_message, "I can't find or don't have access to that channel", ctx);
            }
        }

        let gid = new_message
            .guild_id
            .expect("no guild id attached to message");
        self.notify_channels.lock().await.insert(gid, cid);
    }
}

#[tokio::main]
#[tracing::instrument]
async fn main() -> Result<()> {
    color_eyre::install().expect("couldn't initialise eyre");
    tracing_subscriber::fmt::init();

    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN not set");
    let intents = GatewayIntents::GUILD_MEMBERS | GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;

    let handler = Handler {
        notify_channels: Arc::new(Mutex::new(HashMap::new())),
    };

    info!("starting client");
    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .wrap_err("error creating client")?;

    client.start().await.wrap_err("client start error")?;
    unreachable!("client exited")
}
