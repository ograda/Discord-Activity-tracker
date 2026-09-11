use anyhow::{Context, Result};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};

use crate::resolver::ResolvedActivity;

pub struct DiscordPresence {
    application_id: String,
    client: Option<DiscordIpcClient>,
}

impl DiscordPresence {
    pub fn new(application_id: String) -> Self {
        Self {
            application_id,
            client: None,
        }
    }

    pub fn set(&mut self, resolved: &ResolvedActivity, started_at: i64) -> Result<()> {
        self.ensure_connected()?;

        let payload = activity::Activity::new()
            .details(&resolved.details)
            .state(&resolved.state)
            .timestamps(activity::Timestamps::new().start(started_at));

        if let Some(client) = &mut self.client {
            if let Err(error) = client.set_activity(payload) {
                self.disconnect();
                return Err(error).context("could not update Discord Rich Presence");
            }
        }

        Ok(())
    }

    pub fn set_hidden(&mut self) -> Result<()> {
        self.ensure_connected()?;

        let payload = activity::Activity::new()
            .details("Activities hidden")
            .state("0 activities displayed");

        if let Some(client) = &mut self.client {
            if let Err(error) = client.set_activity(payload) {
                self.disconnect();

                return Err(error).context("could not publish hidden Discord presence");
            }
        }

        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        if self.client.is_none() {
            return Ok(());
        }

        if let Some(client) = &mut self.client {
            if let Err(error) = client.clear_activity() {
                self.disconnect();
                return Err(error).context("could not clear Discord Rich Presence");
            }
        }

        Ok(())
    }

    fn ensure_connected(&mut self) -> Result<()> {
        if self.client.is_some() {
            return Ok(());
        }

        let mut client = DiscordIpcClient::new(&self.application_id);
        client
            .connect()
            .context("could not connect to the Discord desktop client")?;
        self.client = Some(client);
        Ok(())
    }

    fn disconnect(&mut self) {
        if let Some(mut client) = self.client.take() {
            let _ = client.close();
        }
    }
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        self.disconnect();
    }
}
