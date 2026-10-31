use super::*;
impl Shark {
    pub(in crate::app) fn render_recap_mon(&self, mon: &Mon, size: f32) -> AnyElement {
        let key = mon_sprite_identity(mon, mon.species_id());
        let id = format!(
            "usage-recap-sprite-{}-{}-{}",
            key.0,
            key.1,
            key.2.as_ref().map(|f| f.0.as_str()).unwrap_or("default")
        );
        let path = self.pokemon_ui.thumbnails.get(&key).or_else(|| {
            (self.pokemon_ui.sprite_key.as_ref() == Some(&key))
                .then_some(self.pokemon_ui.sprite.as_ref())
                .flatten()
        });
        div()
            .id(id)
            .size(px(size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .when_some(path, |d, path| {
                d.child(
                    if let Some(asset) = crate::pokemon::sprites::cached_memory(path) {
                        images::paint_frame(images::render_image(asset), 0, size)
                    } else {
                        images::sized(path, size).into_any_element()
                    },
                )
            })
            .when(path.is_none(), |d| d.child("✦"))
            .test_support()
            .into_any_element()
    }
    pub(in crate::app) fn load_recap_mons(&mut self, mons: &[Mon], cx: &mut Context<Self>) {
        for mon in mons {
            self.load_pokemon_thumbnail(mon_sprite_identity(mon, mon.species_id()), cx);
        }
    }
}
