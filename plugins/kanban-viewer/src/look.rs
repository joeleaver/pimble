//! How this person has the board on this device: lane sizes and folded
//! columns, remembered by the host and never written to the store.

use super::*;

/// How big a lane is drawn (kobayban's three): one bar with its name and
/// count, about three cards a column with the rest scrolled, or everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Size {
    Collapsed,
    Minimized,
    Expanded,
}

impl Size {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Size::Collapsed => "collapsed",
            Size::Minimized => "minimized",
            Size::Expanded => "expanded",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Size> {
        match s {
            "collapsed" => Some(Size::Collapsed),
            "minimized" => Some(Size::Minimized),
            "expanded" => Some(Size::Expanded),
            _ => None,
        }
    }
}

/// How this person has the board on this device: the lanes they sized (by
/// [`lane_key`]) and the columns they folded (by [`column_key`], so a folded
/// "Done" is folded in every lane). Kept by the host
/// (`Host::remember`), never in the store: someone else's board is
/// not folded because this one is.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Look {
    pub(crate) lanes: BTreeMap<String, Size>,
    pub(crate) folded: Vec<String>,
}

impl Look {
    pub(crate) fn parse(text: &str) -> Look {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return Look::default() };
        let lanes = value
            .get("lanes")
            .and_then(|l| l.as_object())
            .map(|l| l.iter().filter_map(|(k, v)| Some((k.clone(), Size::parse(v.as_str()?)?))).collect())
            .unwrap_or_default();
        let folded = value
            .get("folded")
            .and_then(|f| f.as_array())
            .map(|f| f.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        Look { lanes, folded }
    }

    pub(crate) fn to_json(&self) -> String {
        let lanes: serde_json::Map<String, serde_json::Value> = self.lanes.iter().map(|(k, v)| (k.clone(), serde_json::Value::from(v.name()))).collect();
        serde_json::json!({ "lanes": lanes, "folded": self.folded }).to_string()
    }
}

/// A lane's key in the [`Look`]: its node on the board, or "top" for the
/// unnamed lane.
pub(crate) fn lane_key(lane: &Lane) -> String {
    lane.node.map_or_else(|| "top".to_string(), key_of)
}

impl Kanban {
    /// How big `lane` is drawn, tracked: a board with one lane shows it
    /// whole; with several, each is minimized until the person sizes it.
    pub(crate) fn size_of(&self, lane: &Lane, lanes: usize) -> Size {
        if lanes < 2 {
            return Size::Expanded;
        }
        self.look.with(|l| l.lanes.get(&lane_key(lane)).copied()).unwrap_or(Size::Minimized)
    }

    /// Change how the board looks, and have the host remember it.
    pub(crate) fn relook(&self, f: impl FnOnce(&mut Look)) {
        let mut look = untracked(|| self.look.get());
        f(&mut look);
        if untracked(|| self.look.get()) != look {
            self.host.remember(look.to_json());
            self.look.set(look);
        }
    }

    pub(crate) fn set_size(&self, lane: &Lane, size: Size) {
        let key = lane_key(lane);
        self.relook(|l| {
            l.lanes.insert(key, size);
        });
    }

    /// Fold or unfold the column named `name`, in every lane.
    pub(crate) fn toggle_fold(&self, name: &str) {
        let key = column_key(name);
        self.relook(|l| match l.folded.iter().position(|f| *f == key) {
            Some(i) => {
                l.folded.remove(i);
            }
            None => l.folded.push(key),
        });
    }

    /// Whether the column named `name` is folded, tracked.
    pub(crate) fn folded(&self, name: &str) -> bool {
        let key = column_key(name);
        self.look.with(|l| l.folded.contains(&key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_look_is_remembered_as_json_and_read_back() {
        let mut look = Look::default();
        look.lanes.insert("top".into(), Size::Collapsed);
        look.lanes.insert("s/n".into(), Size::Expanded);
        look.folded.push("done".into());
        assert_eq!(Look::parse(&look.to_json()), look);
        // Anything else reads as the board's own look.
        assert_eq!(Look::parse("not json"), Look::default());
        assert_eq!(Look::parse(r#"{"lanes":{"top":"sideways"}}"#), Look::default());
    }
}
