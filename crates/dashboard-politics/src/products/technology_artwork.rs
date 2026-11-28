use dashboard_base::products::{LoadContext, Product, artwork};
use dashboard_core::dto::Artwork;
use dashboard_base::Source;
use dashboard_text::metadata::check_cancel;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, sync::atomic::AtomicBool};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

pub struct TechnologyArtwork;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TechnologyArtworkParams {
    pub tokens: Vec<String>,
    #[doc = " Repeated definitions have separate recorded indices; null means an unindexed use."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub recorded: Option<BTreeMap<String, Vec<Option<u32>>>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct TechnologyArtworkReply {
    pub items: Vec<Artwork>,
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct State {
    artwork: Option<artwork::Context>,
}

impl Product for TechnologyArtwork {
    const KEY: &'static str = "technology-artwork";
    const CONTENT_DEPENDENT: bool = true;
    const INTERACTIVE: bool = true;
    const PREPARE: Option<dashboard_base::products::read_ahead::Prepare<Self::Params>> =
        Some(|source, _, load| artwork::prepare_artwork(source, load));
    type Params = TechnologyArtworkParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![tokens, recorded, "recorded.*[]"];
    type Reply = TechnologyArtworkReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![
            TechnologyArtworkParams::decl(),
            TechnologyArtworkReply::decl(),
        ]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        if params.tokens.is_empty()
            || params
                .tokens
                .iter()
                .any(|token| token.is_empty() || token.contains('\0'))
        {
            return Err("Invalid technology artwork tokens");
        }
        if params.recorded.as_ref().is_some_and(|recorded| {
            recorded.iter().any(|(token, indices)| {
                !params.tokens.contains(token) || indices.is_empty() || indices.contains(&Some(0))
            })
        }) {
            return Err("Invalid technology artwork templates");
        }
        Ok(())
    }

    fn load(
        source: &Source,
        params: &Self::Params,
        context: &LoadContext,
        cancelled: &AtomicBool,
    ) -> io::Result<Self::Reply> {
        check_cancel(cancelled)?;
        let state = source.state::<State>();
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        let art = artwork::ensure(
            &mut state.artwork,
            source,
            context.content()?,
            artwork::Kind::Technology,
        )?;
        let recorded = |token: &str| {
            params
                .recorded
                .as_ref()
                .and_then(|recorded| recorded.get(token))
                .map_or(&[][..], Vec::as_slice)
        };
        let (items, warnings) = art.recorded_catalogue(
            params
                .tokens
                .iter()
                .map(|token| (token.as_str(), recorded(token)))
                .collect::<BTreeMap<_, _>>(),
            cancelled,
        )?;
        Ok(TechnologyArtworkReply { items, warnings })
    }
}
