//! Publish caught Pokémon and cached portraits to existing chat entities.
use super::*;

impl Shark {
    pub(in crate::app) fn refresh_chat_pokemon(&mut self, cx: &mut Context<Self>) {
        // Most frames have no background agents. Avoid constructing the collection then.
        let roster = if self.state.pokemon_disabled
            || !self
                .chats
                .values()
                .any(|chat| !chat.read(cx).background.is_empty())
        {
            std::sync::Arc::new(vec![])
        } else {
            self.pokemon_ui
                .activity_roster
                .get_or_insert_with(|| {
                    std::sync::Arc::new(crate::chat::activity::roster(&self.state.pokemon))
                })
                .clone()
        };
        let chats: Vec<_> = self.chats.values().cloned().collect();
        for chat in chats {
            let missing = chat.update(cx, |chat, cx| chat.sync_activity_roster(&roster, cx));
            for key in missing {
                let path = self.pokemon_ui.thumbnails.get(&key).cloned();
                let image = path.as_ref().and_then(|path| {
                    images::cached_frame(path, Duration::MAX).or_else(|| {
                        crate::pokemon::sprites::cached_memory(path)
                            .map(|asset| images::render_capped(asset, Duration::MAX))
                    })
                });
                if let Some(image) = image {
                    chat.update(cx, |chat, cx| chat.set_activity_portrait(key, image, cx));
                } else {
                    // An evicted decoded bitmap is warmed on a worker, never during rendering.
                    self.pokemon_ui.thumbnails.remove(&key);
                    self.load_pokemon_thumbnail(key, cx);
                }
            }
        }
    }
}
