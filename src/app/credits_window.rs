//! Help > Credits & licenses (its data is `crate::credits`).

use std::time::Duration;

use eframe::egui;

use crate::credits::{self, ContributorsState};
use crate::updater;

use super::{App, fit_width};
use super::widgets::Culled;

impl App {
    /// Help > Credits & licenses: contributors (from GitHub), the font, Lua,
    /// and every library in the app with its license text.
    pub(super) fn credits_ui(&mut self, ctx: &egui::Context) {
        if !self.credits_open || self.mobile() {
            return;
        }
        let mut close = false;
        let response = egui::Modal::new(egui::Id::unique("credits")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 660.0));
            ui.heading("Credits & licenses");
            self.credits_body_ui(ui, false);
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if close || response.should_close() {
            self.credits_open = false;
        }
    }

    /// Contributors (from GitHub), the font, Lua, and every library in
    /// the app with its license text. On a page the list fills what's
    /// left.
    pub(super) fn credits_body_ui(&mut self, ui: &mut egui::Ui, page: bool) {
        self.contributors.fetch_once();
        let contributors = self.contributors.state();
        if matches!(contributors, ContributorsState::Fetching) {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        let third_party = credits::third_party();
        let repo_url = format!("https://github.com/{}", updater::REPO);
        ui.horizontal_wrapped(|ui| {
            ui.weak(format!("synththing v{}", crate::build_info::describe()));
            ui.hyperlink_to(&repo_url, &repo_url);
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("synththing itself is released under the WTFPL: do whatever you want with it.");
            ui.hyperlink_to("License", format!("{repo_url}/blob/master/LICENSE"));
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("credits_scroll")
            .max_height(if page { f32::INFINITY } else { 480.0 })
            .auto_shrink([false, !page])
            .show(
            ui,
            |ui| {
                ui.strong("Contributors");
                match &contributors {
                    ContributorsState::NotFetched | ContributorsState::Fetching => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.weak("Loading from GitHub...");
                        });
                    }
                    ContributorsState::Done(list) if list.is_empty() => {
                        ui.weak("(none listed yet)");
                    }
                    ContributorsState::Done(list) => {
                        for contributor in list {
                            ui.horizontal(|ui| {
                                ui.hyperlink_to(&contributor.login, &contributor.html_url);
                                let s = if contributor.contributions == 1 { "" } else { "s" };
                                ui.weak(format!("{} commit{s}", contributor.contributions));
                            });
                        }
                    }
                    ContributorsState::Failed(error) => {
                        ui.weak(format!("Couldn't load the list ({error})."));
                        ui.hyperlink_to("See them on GitHub", format!("{repo_url}/graphs/contributors"));
                    }
                }

                ui.separator();
                ui.strong("Fonts");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Script text uses Monogram by Vinícius Menézio (datagoblin), released to the public domain (CC0). Attribution isn't required, but it's given gladly.");
                    ui.hyperlink_to("datagoblin.itch.io/monogram", "https://datagoblin.itch.io/monogram");
                });
                egui::CollapsingHeader::new("Monogram's credits").id_salt("credits_monogram").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::MONOGRAM_CREDITS.trim()).monospace());
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Japanese and Korean text uses Noto Sans JP and Noto Sans KR by Google and Adobe, under the SIL Open Font License 1.1.");
                    ui.hyperlink_to("github.com/notofonts/noto-cjk", "https://github.com/notofonts/noto-cjk");
                });
                egui::CollapsingHeader::new("Noto Sans CJK's license").id_salt("credits_noto_cjk").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::NOTO_CJK_LICENSE.trim()).monospace());
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Emoji use Noto Emoji by Google, under the SIL Open Font License 1.1.");
                    ui.hyperlink_to("github.com/googlefonts/noto-emoji", "https://github.com/googlefonts/noto-emoji");
                });
                egui::CollapsingHeader::new("Noto Emoji's license").id_salt("credits_noto_emoji").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::NOTO_EMOJI_LICENSE.trim()).monospace());
                });

                ui.separator();
                ui.strong("Starter pack soundfonts");
                ui.horizontal_wrapped(|ui| {
                    ui.label("GeneralUser GS by S. Christian Collins, used and hosted as its license asks.");
                    ui.hyperlink_to("schristiancollins.com/generaluser", "https://www.schristiancollins.com/generaluser");
                });
                egui::CollapsingHeader::new("GeneralUser GS's license").id_salt("credits_generaluser").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::GENERALUSER_LICENSE.trim()).monospace());
                });
                ui.label(
                    "TimGM6mb by Tim Brechbill (2004), with changes by David Bolton (2010), under the GNU GPL \
                     version 2. The soundfont file is its own source.",
                );
                egui::CollapsingHeader::new("TimGM6mb's license (GPL-2.0)").id_salt("credits_timgm6mb").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::TIMGM6MB_LICENSE.trim()).monospace());
                });
                ui.weak("The starter songs are synththing's own arrangements of public-domain music.");

                ui.separator();
                ui.strong("Lua");
                ui.label("Visualizer scripts run on Lua 5.4, by Lua.org, PUC-Rio (MIT license), through mlua.");
                egui::CollapsingHeader::new("Lua's license").id_salt("credits_lua").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::LUA_LICENSE.trim()).monospace());
                });

                ui.separator();
                ui.strong(format!("Libraries ({})", third_party.packages.len()));
                ui.weak("Everything compiled into this build, with its license. Click one for its license text.");
                ui.add(
                    egui::TextEdit::singleline(&mut self.credits_search)
                        .hint_text("search by name or license...")
                        .desired_width(240.0),
                );
                let search = self.credits_search.to_lowercase();
                // (Hundreds of packages: the page cost 8-11 ms every frame
                // on a phone drawn whole.)
                let mut culled = Culled::begin(ui, "credits_packages");
                for package in &third_party.packages {
                    let matches = search.is_empty()
                        || package.name.to_lowercase().contains(&search)
                        || package.license.to_lowercase().contains(&search);
                    if !matches {
                        continue;
                    }
                    // (A header of its own: a collapsing header's text never
                    // wraps or shortens, and long license names ran off a
                    // phone's screen.)
                    let id = ui.make_persistent_id(("credits_package", &package.name, &package.version));
                    culled.block(ui, id, false, |ui| {
                        let mut clicked = false;
                        let mut header = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                            .show_header(ui, |ui| {
                                let text = format!("{} {}   {}", package.name, package.version, package.license);
                                clicked = ui.add(egui::Label::new(text).truncate().sense(egui::Sense::click())).clicked();
                            });
                        if clicked {
                            header.toggle();
                        }
                        header.body(|ui| {
                            if !package.authors.is_empty() {
                                ui.weak(format!("By {}", package.authors.join(", ")));
                            }
                            if let Some(repository) = &package.repository {
                                ui.hyperlink_to(repository, repository);
                            }
                            for text in third_party.texts_for(package) {
                                ui.label(egui::RichText::new(text).monospace().small());
                            }
                        });
                    });
                }
                culled.end(ui);
            },
        );
    }
}

