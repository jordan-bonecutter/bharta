use anyhow::Result;
use kbvm::{
    Components, GroupIndex, Keycode, ModifierMask,
    lookup::LookupTable,
    xkb::{
        Context,
        compose::{ComposeTable, FeedResult, State},
    },
};
use xkeysym::Keysym;

#[derive(Clone)]
pub struct KeyEvent {
    pub keysym: Keysym,
    pub utf8: Option<String>,
}

pub struct Keyboard {
    table: Option<LookupTable>,
    pub components: Components,
    compose: ComposeTable,
    compose_state: State,
}

impl Default for Keyboard {
    fn default() -> Self {
        let context = Context::default();
        let compose = context
            .compose_table_builder()
            .build(Vec::new())
            .unwrap_or_else(|| {
                let mut builder = context.compose_table_builder();
                builder.buffer(include_bytes!("../../assets/Compose"));
                builder.build(Vec::new()).expect("Bundled compose rules")
            });
        let compose_state = compose.create_state();
        Self {
            table: None,
            components: Components::default(),
            compose,
            compose_state,
        }
    }
}

impl Keyboard {
    pub fn keymap(&mut self, bytes: &[u8]) -> Result<()> {
        // Wayland supplies a resolved map; no system XKB headers or data are needed.
        let mut builder = Context::builder();
        builder.enable_default_includes(false);
        builder.enable_environment(false);
        let map = builder.build().keymap_from_bytes(
            Vec::new(),
            None,
            bytes.strip_suffix(&[0]).unwrap_or(bytes),
        )?;
        self.table = Some(map.to_builder().build_lookup_table());
        self.components = Components::default();
        self.reset_compose();
        Ok(())
    }
    pub fn reset_compose(&mut self) {
        self.compose_state = self.compose.create_state();
    }
    pub fn modifiers(&mut self, pressed: u32, latched: u32, locked: u32, group: u32) {
        self.components.mods_pressed = ModifierMask(pressed);
        self.components.mods_latched = ModifierMask(latched);
        self.components.mods_locked = ModifierMask(locked);
        self.components.group_locked = GroupIndex(group);
        self.components.update_effective();
    }
    pub fn repeats(&self, raw: u32) -> bool {
        self.table
            .as_ref()
            .is_some_and(|t| t.repeats(Keycode::from_evdev(raw)))
    }
    pub fn event(&mut self, raw_code: u32, pressed: bool) -> Option<KeyEvent> {
        let lookup = self.table.as_ref()?.lookup(
            self.components.group,
            self.components.mods,
            Keycode::from_evdev(raw_code),
        );
        let mut symbols = lookup.into_iter();
        let first = symbols.next()?;
        let mut keysym = first.keysym();
        let mut utf8: String = first
            .char()
            .into_iter()
            .chain(symbols.filter_map(|s| s.char()))
            .collect();
        if pressed {
            match self.compose.feed(&mut self.compose_state, keysym) {
                Some(FeedResult::Pending | FeedResult::Aborted) => utf8.clear(),
                Some(FeedResult::Composed {
                    string,
                    keysym: composed,
                }) => {
                    if let Some(sym) = composed {
                        keysym = sym;
                    }
                    utf8 = string
                        .map(str::to_owned)
                        .unwrap_or_else(|| keysym.char().into_iter().collect());
                }
                None => {}
            }
        }
        Some(KeyEvent {
            keysym: Keysym::new(keysym.0),
            utf8: (!utf8.is_empty()).then_some(utf8),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_modifiers_and_repeat() {
        let mut keyboard = Keyboard::default();
        keyboard
            .keymap(include_bytes!("../../tests/fixtures/us.xkb"))
            .unwrap();
        assert_eq!(keyboard.event(30, true).unwrap().utf8.as_deref(), Some("a"));
        keyboard.modifiers(1, 0, 0, 0);
        assert_eq!(keyboard.event(30, true).unwrap().utf8.as_deref(), Some("A"));
        keyboard.modifiers(0, 0, 2, 0);
        assert_eq!(keyboard.event(30, true).unwrap().utf8.as_deref(), Some("A"));
        keyboard.modifiers(4, 0, 0, 0);
        assert_eq!(keyboard.event(30, true).unwrap().keysym, Keysym::a);
        assert!(keyboard.repeats(30));
        assert!(!keyboard.repeats(42));
    }
    #[test]
    fn non_latin_group_and_latched_modifiers() {
        let map = include_str!("../../tests/fixtures/us.xkb").replace(
            "[               a,               A ]",
            "symbols[Group1]=[a,A], symbols[Group2]=[Cyrillic_a,Cyrillic_A]",
        );
        let mut keyboard = Keyboard::default();
        keyboard.keymap(map.as_bytes()).unwrap();
        keyboard.modifiers(0, 0, 0, 1);
        assert_eq!(keyboard.event(30, true).unwrap().utf8.as_deref(), Some("а"));
        keyboard.modifiers(0, 1, 0, 1);
        assert_eq!(keyboard.event(30, true).unwrap().utf8.as_deref(), Some("А"));
    }
    #[test]
    fn bundled_compose_has_dead_keys_and_sequences() {
        let context = Context::default();
        let mut builder = context.compose_table_builder();
        builder.buffer(include_bytes!("../../assets/Compose"));
        let table = builder.build(Vec::new()).unwrap();
        let mut state = table.create_state();
        assert_eq!(
            table.feed(&mut state, kbvm::syms::dead_acute),
            Some(FeedResult::Pending)
        );
        assert!(matches!(
            table.feed(&mut state, kbvm::syms::e),
            Some(FeedResult::Composed {
                string: Some("é"),
                ..
            })
        ));
        assert_eq!(
            table.feed(&mut state, kbvm::syms::Multi_key),
            Some(FeedResult::Pending)
        );
        assert_eq!(
            table.feed(&mut state, kbvm::syms::o),
            Some(FeedResult::Pending)
        );
        assert!(matches!(
            table.feed(&mut state, kbvm::syms::e),
            Some(FeedResult::Composed {
                string: Some("œ"),
                ..
            })
        ));
    }
}
