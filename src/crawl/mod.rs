//! Safe web crawling helpers for product discovery.

pub mod appstore;
pub mod palette;
pub mod safe_fetch;
pub mod site;

#[allow(unused_imports)]
pub use appstore::{AppStoreInfo, lookup as lookup_app_store, scrape_screenshot_urls};
#[allow(unused_imports)]
pub use palette::dominant_colors;
#[allow(unused_imports)]
pub use safe_fetch::{fetch_bytes, fetch_text};
#[allow(unused_imports)]
pub use site::{SiteSummary, parse_html};
