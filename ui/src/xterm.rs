use js_sys::{Function, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

pub struct Term {
    term: JsValue,
    fit: JsValue,
}

impl Term {
    pub fn mount(el: &HtmlElement) -> Result<Self, JsValue> {
        let win = web_sys::window().ok_or("no window")?;
        let ctor = Reflect::get(&win, &"Terminal".into())?;
        let opts = js_sys::Object::new();
        Reflect::set(&opts, &"cursorBlink".into(), &JsValue::TRUE)?;
        Reflect::set(&opts, &"convertEol".into(), &JsValue::TRUE)?;
        Reflect::set(
            &opts,
            &"fontFamily".into(),
            &"ui-monospace, Menlo, Consolas, monospace".into(),
        )?;
        Reflect::set(&opts, &"fontSize".into(), &13.into())?;
        Reflect::set(&opts, &"rendererType".into(), &"dom".into())?;
        let theme = js_sys::Object::new();
        Reflect::set(&theme, &"background".into(), &"#11100e".into())?;
        Reflect::set(&theme, &"foreground".into(), &"#ece7df".into())?;
        Reflect::set(&theme, &"cursor".into(), &"#c45c26".into())?;
        Reflect::set(&opts, &"theme".into(), &theme)?;
        let term = Reflect::construct(&ctor.dyn_into::<Function>()?, &js_sys::Array::of1(&opts))?;
        let ns = Reflect::get(&win, &"FitAddon".into())?;
        let fit_ctor = Reflect::get(&ns, &"FitAddon".into())?.dyn_into::<Function>()?;
        let fit = Reflect::construct(&fit_ctor, &js_sys::Array::new())?;
        call(&term, "loadAddon", &fit)?;
        call(&term, "open", el)?;
        call0(&fit, "fit")?;
        Ok(Self { term, fit })
    }

    pub fn fit(&self) {
        let _ = call0(&self.fit, "fit");
    }

    pub fn focus(&self) {
        let _ = call0(&self.term, "focus");
    }

    pub fn reset(&self) {
        let _ = call0(&self.term, "reset");
    }

    pub fn write(&self, s: &str) {
        let _ = call(&self.term, "write", &JsValue::from_str(s));
    }

    pub fn size(&self) -> (u16, u16) {
        let cols = Reflect::get(&self.term, &"cols".into())
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(80.0) as u16;
        let rows = Reflect::get(&self.term, &"rows".into())
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(24.0) as u16;
        (cols, rows)
    }

    pub fn on_data(&self, mut f: impl FnMut(String) + 'static) {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
            if let Some(s) = v.as_string() {
                f(s);
            }
        });
        let _ = call(&self.term, "onData", cb.as_ref());
        cb.forget();
    }
}

fn call(obj: &JsValue, name: &str, arg: &JsValue) -> Result<JsValue, JsValue> {
    let f = Reflect::get(obj, &name.into())?.dyn_into::<Function>()?;
    f.call1(obj, arg)
}

fn call0(obj: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let f = Reflect::get(obj, &name.into())?.dyn_into::<Function>()?;
    f.call0(obj)
}
