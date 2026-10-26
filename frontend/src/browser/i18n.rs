//! Translation behavior mirrors the reference runtime: translated
//! text nodes retain their original strings and added nodes are processed too.
use super::{json_body, request};
use js_sys::{Array, Object, WeakMap};
use serde_json::{Map, Value};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{MutationObserver, MutationObserverInit, MutationRecord, Node};

type Translations = Rc<RefCell<Map<String, Value>>>;

#[derive(Clone)]
pub struct I18n {
    map: Translations,
    originals: WeakMap,
    // Keeps the single observer and closure alive for the lifetime of the app.
    _observer: Option<Rc<Observer>>,
}

struct Observer {
    observer: MutationObserver,
    _callback: Closure<dyn FnMut(Array, MutationObserver)>,
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}

impl I18n {
    pub fn new() -> Self {
        let map = Rc::new(RefCell::new(Map::new()));
        let originals = WeakMap::new();
        let observer = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|document| {
                let body = document.body()?;
                let map = map.clone();
                let originals = originals.clone();
                let callback =
                    Closure::<dyn FnMut(Array, MutationObserver)>::new(move |records: Array, _| {
                        for record in records
                            .iter()
                            .filter_map(|v| v.dyn_into::<MutationRecord>().ok())
                        {
                            let nodes = record.added_nodes();
                            for index in 0..nodes.length() {
                                if let Some(node) = nodes.item(index) {
                                    process_node(&node, &map, &originals);
                                }
                            }
                        }
                    });
                let observer = MutationObserver::new(callback.as_ref().unchecked_ref()).ok()?;
                let options = MutationObserverInit::new();
                options.set_child_list(true);
                options.set_subtree(true);
                observer.observe_with_options(&body, &options).ok()?;
                Some(Rc::new(Observer {
                    observer,
                    _callback: callback,
                }))
            });
        Self {
            map,
            originals,
            _observer: observer,
        }
    }

    pub async fn reload(&self, locale: &str) {
        let translations = if locale == "en" {
            Map::new()
        } else {
            let path = format!("/i18n/literals/{locale}.json");
            match request("GET", &path, None, None).await {
                Ok(response) => json_body(&response)
                    .await
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default(),
                Err(_) => Map::new(),
            }
        };
        *self.map.borrow_mut() = translations;
        if let Some(body) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.body())
        {
            process_node(body.as_ref(), &self.map, &self.originals);
        }
    }
}

fn process_node(node: &Node, map: &Translations, originals: &WeakMap) {
    if node.node_type() == Node::TEXT_NODE {
        process_text(node, map, originals);
    } else if node.node_type() == Node::ELEMENT_NODE {
        let Some(document) = node.owner_document() else {
            return;
        };
        let Ok(walker) = document.create_tree_walker_with_what_to_show(node, 4) else {
            return;
        };
        while let Ok(Some(text)) = walker.next_node() {
            process_text(&text, map, originals);
        }
    }
}

fn process_text(node: &Node, map: &Translations, originals: &WeakMap) {
    let Some(current) = node.node_value().filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let Some(parent) = node.parent_element() else {
        return;
    };
    if parent.closest("[data-i18n-skip]").ok().flatten().is_some() {
        return;
    }
    if matches!(
        parent.tag_name().to_ascii_lowercase().as_str(),
        "script"
            | "style"
            | "code"
            | "pre"
            | "colgroup"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "select"
            | "datalist"
            | "optgroup"
    ) {
        return;
    }
    let key: &Object = node.unchecked_ref();
    let original = originals.get(key).as_string().unwrap_or_else(|| {
        originals.set(key, &JsValue::from_str(&current));
        current.clone()
    });
    let translations = map.borrow();
    let translated = translations
        .get(original.trim())
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(&original);
    if translated != current {
        node.set_node_value(Some(translated));
    }
}
