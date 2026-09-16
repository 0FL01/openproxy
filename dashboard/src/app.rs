use leptos::prelude::*;
use leptos_router::{
    components::{Route, Router, Routes},
    path,
};

use crate::{browser::initialize_theme, pages};

#[component]
pub fn App() -> impl IntoView {
    provide_context(initialize_theme());

    view! {
        <Router>
            <Routes fallback=pages::not_found::NotFound>
                <Route path=path!("/") view=pages::endpoint::EndpointPage />
                <Route path=path!("/landing") view=pages::placeholder::LandingPage />
                <Route path=path!("/login") view=pages::login::LoginPage />
                <Route path=path!("/callback") view=pages::callback::CallbackPage />
                <Route path=path!("/dashboard") view=pages::endpoint::EndpointPage />
                <Route path=path!("/dashboard/endpoint") view=pages::endpoint::EndpointPage />
                <Route path=path!("/dashboard/providers") view=pages::providers::ProvidersPage />
                <Route path=path!("/dashboard/providers/new") view=pages::providers::ProviderNewPage />
                <Route path=path!("/dashboard/providers/:id") view=pages::provider_detail::ProviderDetailPage />
                <Route path=path!("/dashboard/cli-tools") view=pages::cli_tools::CliToolsPage />
                <Route path=path!("/dashboard/cli-tools/:id") view=pages::opencode::CliToolPage />
                <Route path=path!("/dashboard/basic-chat") view=pages::chat::BasicChatPage />
                <Route path=path!("/dashboard/quota") view=pages::data_pages::QuotaPage />
                <Route path=path!("/dashboard/profile") view=pages::profile::ProfilePage />
                <Route path=path!("/dashboard/skills") view=pages::data_pages::SkillsPage />
                <Route path=path!("/dashboard/translator") view=pages::translator::TranslatorPage />
                <Route path=path!("/dashboard/console-log") view=pages::console_log::ConsoleLogPage />
                <Route path=path!("/dashboard/db-backups") view=pages::data_pages::DbBackupsPage />
                <Route path=path!("/dashboard/proxy-pools") view=pages::data_pages::ProxyPoolsPage />
                <Route path=path!("/dashboard/logs") view=pages::data_pages::ApplicationLogsPage />
            </Routes>
        </Router>
    }
}
