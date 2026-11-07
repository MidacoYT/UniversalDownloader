//! Lecteur de trailers intégré : une WebView2 enfant (wry) qui lit les manifests DASH (.mpd)
//! via dash.js. Le décodage, l'audio et les contrôles sont gérés par le moteur Chromium.
use std::path::PathBuf;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::raw_window_handle::HasWindowHandle;
use wry::{WebContext, WebView, WebViewBuilder};
#[cfg(windows)]
use wry::WebViewBuilderExtWindows;

const DASH_JS: &str = "https://cdn.jsdelivr.net/npm/dashjs@4.7.4/dist/dash.all.min.js";

pub struct TrailerPlayer {
    pub title: String,
    url: Option<String>,
    webview: Option<WebView>,
    context: WebContext,
    last_rect: Option<eframe::egui::Rect>,
    pub error: Option<String>,
}

impl TrailerPlayer {
    pub fn new() -> Self {
        let data_dir = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("UniversalDownloader").join("webview2"));
        Self {
            title: String::new(),
            url: None,
            webview: None,
            context: WebContext::new(data_dir),
            last_rect: None,
            error: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.url.is_some()
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub fn open(&mut self, title: String, url: String) {
        self.close();
        self.title = title;
        self.url = Some(url);
    }

    pub fn close(&mut self) {
        self.webview = None; // détruit la WebView (et arrête la lecture)
        self.url = None;
        self.last_rect = None;
        self.error = None;
    }

    /// Crée la WebView au besoin, puis la cale sur `rect` (points egui).
    pub fn sync<W: HasWindowHandle>(&mut self, window: &W, zoom: f32, rect: eframe::egui::Rect) {
        let bounds = wry::Rect {
            position: LogicalPosition::new(
                (rect.min.x * zoom) as f64,
                (rect.min.y * zoom) as f64,
            )
            .into(),
            size: LogicalSize::new((rect.width() * zoom) as f64, (rect.height() * zoom) as f64)
                .into(),
        };

        if let Some(webview) = &self.webview {
            if self.last_rect != Some(rect) {
                let _ = webview.set_bounds(bounds);
                self.last_rect = Some(rect);
            }
            return;
        }
        let (Some(url), None) = (self.url.clone(), &self.error) else {
            return;
        };

        let builder = WebViewBuilder::new_with_web_context(&mut self.context)
            .with_bounds(bounds)
            .with_background_color((10, 10, 18, 255))
            .with_html(player_html(&url));
        #[cfg(windows)]
        let builder = builder.with_additional_browser_args(
            "--autoplay-policy=no-user-gesture-required \
             --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection",
        );
        match builder.build_as_child(window) {
            Ok(webview) => {
                self.webview = Some(webview);
                self.last_rect = Some(rect);
            }
            Err(error) => {
                self.error = Some(format!(
                    "Could not start the video player (is the WebView2 runtime installed?)\n{error}"
                ));
            }
        }
    }
}

fn player_html(url: &str) -> String {
    let js_url = serde_json::to_string(url)
        .unwrap_or_else(|_| "\"\"".into())
        .replace("</", "<\\/");
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;height:100%;background:#0a0a12;overflow:hidden}}
video{{width:100%;height:100%;background:#000;outline:none}}
#err{{position:absolute;inset:0;display:none;align-items:center;justify-content:center;
color:#9494a8;font:14px sans-serif;text-align:center;padding:24px}}
</style></head><body>
<video id="v" controls autoplay playsinline></video><div id="err"></div>
<script src="{DASH_JS}"></script>
<script>
const url = {js_url};
const v = document.getElementById('v');
function fail(m) {{ const e = document.getElementById('err'); e.textContent = m; e.style.display = 'flex'; }}
if (/\.mpd(\?|$)/i.test(url) && window.dashjs) {{
  const p = dashjs.MediaPlayer().create();
  p.initialize(v, url, true);
  p.on('error', () => fail('Playback error'));
}} else {{
  v.src = url;
  v.onerror = () => fail('Unsupported video source');
}}
</script></body></html>"#
    )
}
