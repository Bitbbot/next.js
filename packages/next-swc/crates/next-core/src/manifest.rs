use anyhow::{Context, Result};
use indexmap::IndexMap;
use mime::{APPLICATION_JAVASCRIPT_UTF_8, APPLICATION_JSON};
use serde::Serialize;
use turbo_tasks::{
    graph::{GraphTraversal, NonDeterministic},
    ReadRef, Vc,
};
use turbopack_binding::{
    turbo::{tasks::TryJoinIterExt, tasks_fs::File},
    turbopack::{
        core::{asset::AssetContent, introspect::Introspectable},
        dev_server::source::{
            ContentSource, ContentSourceContent, ContentSourceData, ContentSourceResult,
        },
        node::render::{
            node_api_source::NodeApiContentSource, rendered_source::NodeRenderContentSource,
        },
    },
};

use crate::{
    embed_js::next_js_file,
    next_config::{NextConfig, Rewrites},
    util::get_asset_path_from_pathname,
};

/// A content source which creates the next.js `_devPagesManifest.json` and
/// `_devMiddlewareManifest.json` which are used for client side navigation.
#[turbo_tasks::value(shared)]
pub struct DevManifestContentSource {
    pub page_roots: Vec<Vc<Box<dyn ContentSource>>>,
    pub next_config: Vc<NextConfig>,
}

#[turbo_tasks::value_impl]
impl DevManifestContentSource {
    /// Recursively find all routes in the `page_roots` content sources.
    #[turbo_tasks::function]
    async fn find_routes(self: Vc<Self>) -> Result<Vc<Vec<String>>> {
        let this = &*self.await?;

        async fn content_source_to_pathname(
            content_source: Vc<Box<dyn ContentSource>>,
        ) -> Result<Option<ReadRef<String>>> {
            // TODO This shouldn't use casts but an public api instead
            if let Some(api_source) =
                Vc::try_resolve_downcast_type::<NodeApiContentSource>(content_source).await?
            {
                return Ok(Some(api_source.get_pathname().await?));
            }

            if let Some(page_source) =
                Vc::try_resolve_downcast_type::<NodeRenderContentSource>(content_source).await?
            {
                return Ok(Some(page_source.get_pathname().await?));
            }

            Ok(None)
        }

        async fn get_content_source_children(
            content_source: Vc<Box<dyn ContentSource>>,
        ) -> Result<Vec<Vc<Box<dyn ContentSource>>>> {
            Ok(content_source.get_children().await?.clone_value())
        }

        let routes = NonDeterministic::new()
            .visit(this.page_roots.iter().copied(), get_content_source_children)
            .await
            .completed()?
            .into_iter()
            .map(content_source_to_pathname)
            .try_join()
            .await?;
        let mut routes = routes
            .into_iter()
            .flatten()
            .map(|route| route.clone_value())
            .collect::<Vec<_>>();

        routes.sort_by_cached_key(|s| s.split('/').map(PageSortKey::from).collect::<Vec<_>>());
        routes.dedup();

        Ok(Vc::cell(routes))
    }

    /// Recursively find all pages in the `page_roots` content sources
    /// (excluding api routes).
    #[turbo_tasks::function]
    async fn find_pages(self: Vc<Self>) -> Result<Vc<Vec<String>>> {
        let routes = &*self.find_routes().await?;

        // we don't need to sort as it's already sorted by `find_routes`
        let pages = routes
            .iter()
            .filter(|s| !s.starts_with("/api"))
            .cloned()
            .collect();

        Ok(Vc::cell(pages))
    }

    /// Create a build manifest with all pages.
    #[turbo_tasks::function]
    async fn create_build_manifest(self: Vc<Self>) -> Result<Vc<String>> {
        let this = &*self.await?;

        let sorted_pages = &*self.find_pages().await?;
        let routes = sorted_pages
            .iter()
            .map(|pathname| {
                (
                    pathname,
                    vec![format!(
                        "_next/static/chunks/pages{}",
                        get_asset_path_from_pathname(pathname, ".js")
                    )],
                )
            })
            .collect();

        let manifest = BuildManifest {
            rewrites: this.next_config.rewrites().await?,
            sorted_pages,
            routes,
        };

        let manifest = next_js_file("entry/manifest/buildManifest.js")
            .await?
            .as_content()
            .context("embedded buildManifest file missing")?
            .content()
            .to_str()?
            .replace("$$MANIFEST$$", &serde_json::to_string(&manifest)?);

        Ok(Vc::cell(manifest))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildManifest<'a> {
    #[serde(rename = "__rewrites")]
    rewrites: ReadRef<Rewrites>,
    sorted_pages: &'a Vec<String>,

    #[serde(flatten)]
    routes: IndexMap<&'a String, Vec<String>>,
}

#[turbo_tasks::value_impl]
impl ContentSource for DevManifestContentSource {
    #[turbo_tasks::function]
    async fn get(
        self: Vc<Self>,
        path: String,
        _data: turbo_tasks::Value<ContentSourceData>,
    ) -> Result<Vc<ContentSourceResult>> {
        let manifest_file = match path {
            "_next/static/development/_devPagesManifest.json" => {
                let pages = &*self.find_routes().await?;

                File::from(serde_json::to_string(&serde_json::json!({
                    "pages": pages,
                }))?)
                .with_content_type(APPLICATION_JSON)
            }
            "_next/static/development/_buildManifest.js" => {
                let build_manifest = &*self.create_build_manifest().await?;

                File::from(build_manifest.as_str()).with_content_type(APPLICATION_JAVASCRIPT_UTF_8)
            }
            "_next/static/development/_devMiddlewareManifest.json" => {
                // If there is actual middleware, this request will have been handled by the
                // node router in next-core/js/src/entry/router.ts and
                // next/src/server/lib/route-resolver.ts.
                // If we've reached this point, then there is no middleware and we need to
                // respond with an empty `MiddlewareMatcher[]`.
                File::from("[]").with_content_type(APPLICATION_JSON)
            }
            _ => return Ok(ContentSourceResult::not_found()),
        };

        Ok(ContentSourceResult::exact(Vc::upcast(
            ContentSourceContent::static_content(Vc::upcast(AssetContent::from(manifest_file))),
        )))
    }
}

#[turbo_tasks::value_impl]
impl Introspectable for DevManifestContentSource {
    #[turbo_tasks::function]
    fn ty(&self) -> Vc<String> {
        Vc::cell("dev manifest source".to_string())
    }

    #[turbo_tasks::function]
    fn details(&self) -> Vc<String> {
        Vc::cell(
            "provides _devPagesManifest.json, _buildManifest.js and _devMiddlewareManifest.json."
                .to_string(),
        )
    }
}

/// PageSortKey is necessary because the next.js client code looks for matches
/// in the order the pages are sent in the manifest,if they're sorted
/// alphabetically this means \[slug] and \[\[catchall]] routes are prioritized
/// over fixed paths, so we have to override the ordering with this.
#[derive(Ord, PartialOrd, Eq, PartialEq)]
enum PageSortKey {
    Static(String),
    Slug,
    CatchAll,
}

impl From<&str> for PageSortKey {
    fn from(value: &str) -> Self {
        if value.starts_with("[[") && value.ends_with("]]") {
            PageSortKey::CatchAll
        } else if value.starts_with('[') && value.ends_with(']') {
            PageSortKey::Slug
        } else {
            PageSortKey::Static(value.to_string())
        }
    }
}
