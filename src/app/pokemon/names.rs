use super::*;
impl Shark {
    pub(super) fn request_dex_names(&mut self, cx: &mut Context<Self>) {
        if self.state.pokemon_disabled {
            return;
        }
        let bases = self
            .state
            .pokemon
            .collection
            .iter()
            .chain(self.state.pokemon.active.iter())
            .filter(|m| m.needs_names_refresh())
            .map(|m| m.line.base_id)
            .collect::<BTreeSet<_>>();
        for base in bases {
            if self.pokemon_ui.name_pending.len() >= 4 {
                break;
            }
            if self.pokemon_ui.name_pending.contains(&base)
                || self.pokemon_ui.name_failed.contains(&base)
            {
                continue;
            }
            self.pokemon_ui.name_pending.insert(base);
            #[cfg(test)]
            if !self.usage_live {
                continue;
            }
            let generation = self.pokemon_ui.generation;
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { api().line(base) })
                    .await;
                this.update(cx, |s, cx| {
                    s.complete_pokemon_names(base, generation, result, cx)
                })
                .ok();
            })
            .detach();
        }
    }
    pub(super) fn complete_pokemon_names(
        &mut self,
        base: u16,
        generation: u64,
        result: Result<crate::pokemon::EvoLine, String>,
        cx: &mut Context<Self>,
    ) {
        if self.pokemon_ui.generation != generation {
            return;
        }
        self.pokemon_ui.name_pending.remove(&base);
        match result {
            Ok(line) if line.base_id == base => {
                self.state.pokemon.merge_line_names(&line);
                if self
                    .state
                    .pokemon
                    .collection
                    .iter()
                    .chain(self.state.pokemon.active.iter())
                    .any(|m| m.line.base_id == base && m.needs_names_refresh())
                {
                    self.pokemon_ui.name_failed.insert(base);
                }
                self.save_pokemon();
            }
            _ => {
                self.pokemon_ui.name_failed.insert(base);
            }
        }
        self.request_dex_names(cx);
        cx.notify();
    }
}

impl Shark {
    pub(in crate::app) fn cancel_profile_names(&mut self) {
        self.pokemon_ui.resource_generation = self.pokemon_ui.resource_generation.wrapping_add(1);
        self.pokemon_ui.resource_wanted.clear();
        self.pokemon_ui.resource_checked.clear();
    }
    pub(super) fn resource_text(&self, items: &[NameItem]) -> String {
        crate::pokemon::names_client::text(
            items,
            &["en"],
            &self.pokemon_ui.resource_names,
            &self.pokemon_ui.resource_failed,
        )
    }
    pub(super) fn resource_label(&self, kind: NameKind, name: &str) -> String {
        crate::pokemon::names_client::label(
            &NameResource::new(kind, name),
            &["en"],
            &self.pokemon_ui.resource_names,
            &self.pokemon_ui.resource_failed,
        )
    }
    pub(super) fn selected_form_label(
        &self,
        species: u16,
        form: Option<&crate::pokemon::forms::Form>,
    ) -> Option<String> {
        let resource = crate::pokemon::forms::name_resource(species, form)
            .map(|id| NameResource::new(NameKind::PokemonForm, id.to_string()));
        let names = resource
            .as_ref()
            .and_then(|resource| self.pokemon_ui.resource_names.get(resource));
        crate::pokemon::forms::label(species, form, names)
    }
    pub(super) fn sync_profile_names(&mut self, retry: bool, cx: &mut Context<Self>) {
        let mut wanted = BTreeSet::new();
        if self.pokemon_ui.tab == Page::Details && !self.state.pokemon_disabled {
            if let Some(species) = self.pokemon_ui.selected_species {
                if let Some(mon) = self.selected_pokemon_individual(species) {
                    if mon.species_id() == species {
                        if let Some(profile) = &mon.profile {
                            if let Some(name) = &profile.ability_name {
                                wanted.insert(NameResource::new(NameKind::Ability, name));
                            }
                            wanted.extend(
                                profile
                                    .moves
                                    .iter()
                                    .map(|m| NameResource::new(NameKind::Move, &m.name)),
                            );
                        }
                    }
                    if let Some(id) =
                        crate::pokemon::forms::name_resource(species, mon.form.as_ref())
                    {
                        wanted.insert(NameResource::new(NameKind::PokemonForm, id.to_string()));
                    }
                }
                if let Some(details) = self.pokemon_ui.details.get(&species) {
                    wanted.extend(
                        details
                            .types
                            .iter()
                            .map(|name| NameResource::new(NameKind::Type, name)),
                    );
                }
            }
        }
        wanted.retain(NameResource::valid);
        if retry || (wanted.is_empty() && !self.pokemon_ui.resource_wanted.is_empty()) {
            self.pokemon_ui.resource_generation =
                self.pokemon_ui.resource_generation.wrapping_add(1);
        }
        if wanted != self.pokemon_ui.resource_wanted || retry {
            let previous = std::mem::replace(&mut self.pokemon_ui.resource_wanted, wanted);
            if retry {
                self.pokemon_ui.resource_checked.clear();
            } else {
                self.pokemon_ui
                    .resource_checked
                    .retain(|resource| self.pokemon_ui.resource_wanted.contains(resource));
            }
            for resource in &self.pokemon_ui.resource_wanted {
                if retry || !previous.contains(resource) {
                    self.pokemon_ui.resource_failed.remove(resource);
                }
            }
        }
        self.fill_resource_names(cx);
    }
    fn fill_resource_names(&mut self, cx: &mut Context<Self>) {
        for resource in self.pokemon_ui.resource_wanted.clone() {
            if self.pokemon_ui.resource_pending.len() >= 8 {
                break;
            }
            if self.pokemon_ui.resource_pending.contains_key(&resource)
                || self.pokemon_ui.resource_checked.contains(&resource)
            {
                continue;
            }
            let generation = self.pokemon_ui.generation;
            let view = self.pokemon_ui.resource_generation;
            self.pokemon_ui
                .resource_pending
                .insert(resource.clone(), view);
            self.pokemon_ui.resource_checked.insert(resource.clone());
            #[cfg(test)]
            if !self.usage_live {
                continue;
            }
            cx.spawn(async move |this, cx| {
                let requested = resource.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { api().resource_names(&requested) })
                    .await;
                this.update(cx, |s, cx| {
                    s.complete_resource_names(resource, generation, view, result, cx)
                })
                .ok();
            })
            .detach();
        }
    }
    pub(super) fn complete_resource_names(
        &mut self,
        resource: NameResource,
        generation: u64,
        view: u64,
        result: Result<ResourceNames, String>,
        cx: &mut Context<Self>,
    ) {
        if self.pokemon_ui.generation != generation {
            return;
        }
        if self.pokemon_ui.resource_pending.get(&resource) != Some(&view) {
            return;
        }
        self.pokemon_ui.resource_pending.remove(&resource);
        if self.pokemon_ui.resource_generation == view
            && self.pokemon_ui.resource_wanted.contains(&resource)
        {
            match result {
                Ok(names) => {
                    self.pokemon_ui
                        .resource_names
                        .insert(resource.clone(), names);
                    self.pokemon_ui.resource_failed.remove(&resource);
                }
                Err(_) => {
                    self.pokemon_ui.resource_failed.insert(resource);
                }
            }
            // UI snapshots are small labels; the client retains the bounded disk cache.
            while self.pokemon_ui.resource_names.len() > 512 {
                let Some(old) = self
                    .pokemon_ui
                    .resource_names
                    .keys()
                    .find(|resource| !self.pokemon_ui.resource_wanted.contains(*resource))
                    .cloned()
                else {
                    break;
                };
                self.pokemon_ui.resource_names.remove(&old);
            }
        }
        self.fill_resource_names(cx);
        cx.notify();
    }
}
