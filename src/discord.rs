use anyhow::{Context, Result};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};

use crate::{config::Links, resolver::ResolvedActivity};

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

    pub fn set(
        &mut self,
        resolved: &ResolvedActivity,
        started_at: i64,
        links: Option<&Links>,
    ) -> Result<()> {
        self.ensure_connected()?;

        let payload = activity::Activity::new()
            .details(&resolved.details)
            .state(&resolved.state)
            .timestamps(activity::Timestamps::new().start(started_at))
            .buttons(buttons_from_links(links));

        if let Some(client) = &mut self.client {
            if let Err(error) = client.set_activity(payload) {
                self.disconnect();
                return Err(error).context("could not update Discord Rich Presence");
            }
        }

        Ok(())
    }

    pub fn set_hidden(&mut self, links: Option<&Links>) -> Result<()> {
        self.ensure_connected()?;

        let payload = activity::Activity::new()
            .details("Activities hidden")
            .state("0 activities displayed")
            .buttons(buttons_from_links(links));

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

fn buttons_from_links(links: Option<&Links>) -> Vec<activity::Button<'static>> {
    let Some(links) = links else {
        return Vec::new();
    };

    let mut buttons = Vec::with_capacity(2);

    if let Some(download) = links.download() {
        buttons.push(activity::Button::new(
            "Baixar para Windows",
            download.to_owned(),
        ));
    }

    if let Some(source) = links.source() {
        buttons.push(activity::Button::new("Ver código-fonte", source.to_owned()));
    }

    buttons
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        self.disconnect();
    }
}
