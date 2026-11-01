//! Item artwork loads only when the shop or bag opens; render stays memory-only.
use super::*;

impl Shark {
    pub(super) fn load_pokemon_item_art(&mut self, cx: &mut Context<Self>) {
        if self.state.pokemon_disabled {
            return;
        }
        #[cfg(test)]
        if !self.usage_live {
            return;
        }
        for item in [Item::RareCandy, Item::ShinyCharm] {
            let name = item.sprite_name().unwrap();
            if self.pokemon_ui.item_art.contains_key(name)
                || !self.pokemon_ui.item_art_pending.insert(name)
            {
                continue;
            }
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { api().cached_item_sprite(item) })
                    .await;
                _ = this.update(cx, |s, cx| {
                    s.pokemon_ui.item_art_pending.remove(name);
                    if !s.state.pokemon_disabled {
                        if let Ok(asset) = result {
                            s.pokemon_ui.item_art.insert(name, asset);
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(super) fn pokemon_item_art(&self, item: Item, size: f32) -> AnyElement {
        let artwork = item
            .sprite_name()
            .and_then(|name| self.pokemon_ui.item_art.get(name))
            .and_then(|asset| {
                images::render_capped(asset.clone(), Duration::ZERO);
                images::sized(&asset.path, size)
            });
        div()
            .id(format!(
                "pokemon-item-art-{}",
                item.sprite_name().unwrap_or("mint")
            ))
            .w(px(size))
            .h(px(size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .role(Role::Image)
            .aria_label(item.name())
            .child(
                artwork
                    .map(|image| image.into_any_element())
                    .unwrap_or_else(|| {
                        div()
                            .text_size(px(size * 0.8))
                            .child(item.fallback_emoji())
                            .into_any_element()
                    }),
            )
            .test_support()
            .into_any_element()
    }

    pub(super) fn pokemon_egg_art(&self, size: f32) -> AnyElement {
        div()
            .id("pokemon-egg-art")
            .w(px(size))
            .h(px(size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .role(Role::Image)
            .aria_label("Fresh egg")
            .text_size(px(size * 0.8))
            .child("🥚")
            .into_any_element()
    }
}
