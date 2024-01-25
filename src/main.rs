use std::collections::HashMap;
use std::sync::Arc;

use tracing::{error, instrument};

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

macro_rules! reply_and_return {
    ($orig_msg:expr, $content:expr, $ctx:expr) => {{
        match $orig_msg.reply_mention(&$ctx, $content).await {
            Ok(_) => (),
            Err(e) => {
                error!("couldn't reply to message: {e}");
                return;
            }
        }
        return;
    }};
}

#[async_trait]
impl EventHandler for Handler {
    #[instrument]
    async fn guild_member_removal(
        &self,
        ctx: Context,
        guild_id: GuildId,
        user: User,
        _member_data: Option<Member>,
    ) {
        let notify_channels = self.notify_channels.lock().await;
        let Some(notify_cid) = notify_channels.get(&guild_id) else {
            error!("no notify channel set for {guild_id}");
            return;
        };
        let guild_channels = match guild_id.channels(&ctx.http).await {
            Ok(c) => c,
            Err(e) => {
                error!("error getting channels for {guild_id}: {e}");
                return;
            }
        };
        let to_notif = guild_channels
            .get(notify_cid)
            .ok_or_else(|| format!("guild {guild_id} doesn't have a channel {notify_cid}"))
            .unwrap();

        let content = format!("{} ({}) has left the server", user.name, user.id);
        match to_notif.say(ctx, content).await {
            Ok(_) => (),
            Err(e) => {
                error!("couldn't send message to channel {notify_cid} in guild {guild_id}: {e}")
            }
        }
    }

    #[instrument]
    async fn message(&self, ctx: Context, new_message: Message) {
        let Some(content) = new_message.content.strip_prefix(COMMAND_PREFIX) else {
            return;
        };

        let Some(cid_lit) = content.strip_prefix("notifchan ") else {
            reply_and_return!(new_message, "bad command", ctx);
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
                match new_message
                    .reply_mention(&ctx, "I can't find or don't have access to that channel")
                    .await
                {
                    Ok(_) => (),
                    Err(e) => {
                        error!("couldn't send message in reply: {e}");
                        return;
                    }
                }
                return;
            }
        }

        let gid = new_message
            .guild_id
            .expect("no guild id attached to message");
        self.notify_channels.lock().await.insert(gid, cid);
    }
}

#[tokio::main]
#[instrument]
async fn main() {
    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN not set");
    let intents = GatewayIntents::GUILD_MEMBERS | GatewayIntents::GUILD_MESSAGES;

    let handler = Handler {
        notify_channels: Arc::new(Mutex::new(HashMap::new())),
    };

    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .expect("error creating client");

    if let Err(e) = client.start().await {
        error!("client error: {e}");
    }

    unreachable!("client exited")
}
