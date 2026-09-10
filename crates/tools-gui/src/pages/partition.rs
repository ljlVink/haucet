use crate::app::HaucetApp;
use crate::pages::images::ImageKind;
use crate::util::{human_size, kv, section};
use common::entropy::EntropySummary;
use common::formats::gpt::GptInfo;
use common::formats::secimg::SecImageInfo;
use common::partition::{CertSummary, HarmonySummary, PartitionSummary};
use eframe::egui;
use egui_extras::{Column, TableBuilder};

#[derive(Debug, Default)]
pub struct PartitionPage {
    pub input: String,
    pub summary: Option<PartitionSummary>,
    pub entropy_summary: Option<EntropySummary>,
    pub partition_error: Option<String>,
    inspect_requested: bool,
    active_input: Option<String>,
}

#[derive(serde::Deserialize)]
struct PartitionInspection {
    partition: Option<PartitionSummary>,
    entropy: EntropySummary,
}

impl PartitionPage {
    pub fn select_input(&mut self, input: String) {
        self.input = input;
        self.summary = None;
        self.entropy_summary = None;
        self.partition_error = None;
        self.inspect_requested = !self.input.trim().is_empty();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.set_width(ui.available_width());
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tr!("partition-help")).weak());
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("image-file")).strong());
            let input_response = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text(tr!("image-path-drop-hint"))
                    .desired_width(ui.available_width() - 160.0),
            );
            if input_response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
            {
                self.select_input(self.input.clone());
            }
            if ui.button(tr!("choose-file")).clicked()
                && let Some(path) = app.pick_file(&tr!("choose-image-or-file"), &[])
            {
                self.select_input(path.display().to_string());
            }
        });
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first() {
            self.select_input(path.display().to_string());
        }

        self.start_inspection(app);
        ui.add_space(8.0);

        self.render_results(ui);
    }

    fn render_results(&self, ui: &mut egui::Ui) {
        match (&self.summary, &self.entropy_summary) {
            (Some(summary), Some(entropy)) => {
                if matches!(
                    summary,
                    PartitionSummary::Gpt(_)
                        | PartitionSummary::Rvt(_)
                        | PartitionSummary::SecImage(_)
                ) {
                    self.render(ui, summary);
                    ui.add_space(10.0);
                    crate::pages::entropy::render_summary(ui, entropy);
                    return;
                }
                let available_width = ui.available_width();
                let spacing = ui.spacing().item_spacing.x;
                let column_width = ((available_width - spacing * 2.0 - 1.0) / 2.0).max(0.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(column_width);
                        self.render(ui, summary);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(column_width);
                        crate::pages::entropy::render_summary(ui, entropy);
                    });
                });
            }
            (Some(summary), None) => self.render(ui, summary),
            (None, Some(entropy)) => crate::pages::entropy::render_summary(ui, entropy),
            (None, None) => {}
        }
    }

    pub(crate) fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_image_result(ImageKind::Partition) else {
            return;
        };
        let active_input = self.active_input.take();
        if active_input.as_deref() != Some(self.input.trim()) {
            return;
        }
        let cancelled = result.cancelled;
        self.apply_partition_result(result);
        if let Some(error) = &self.partition_error {
            app.notify(
                if cancelled {
                    egui_notify::ToastLevel::Warning
                } else {
                    egui_notify::ToastLevel::Error
                },
                error.clone(),
            );
        }
        match &self.summary {
            Some(PartitionSummary::HvbWrapped {
                cert: None,
                cert_error,
                ..
            }) => {
                app.notify(egui_notify::ToastLevel::Warning, tr!("certificate-parse-error", "error" => cert_error.clone().unwrap_or_else(|| tr!("unknown-error"))));
            }
            Some(PartitionSummary::SecImage(secimg)) => {
                for warning in &secimg.warnings {
                    app.notify(egui_notify::ToastLevel::Warning, warning.clone());
                }
            }
            Some(PartitionSummary::Gpt(gpt)) if gpt.tables.is_empty() => {
                app.notify(
                    egui_notify::ToastLevel::Warning,
                    tr!("gpt-no-readable-table"),
                );
            }
            _ => {}
        }
    }

    fn start_inspection(&mut self, app: &mut HaucetApp) {
        if !self.inspect_requested || app.job_running() {
            return;
        }
        let image = self.input.trim().to_owned();
        self.inspect_requested = false;
        if image.is_empty() {
            return;
        }
        self.active_input = Some(image.clone());
        app.start_job(crate::worker::JobOp::PartitionInfo { image });
    }

    fn apply_partition_result(&mut self, result: crate::job::JobResult) {
        if !result.ok {
            self.summary = None;
            self.entropy_summary = None;
            self.partition_error = Some(result.summary);
            return;
        }
        if let Some(payload) = result.payload {
            match serde_json::from_value::<PartitionInspection>(payload) {
                Ok(inspection) => {
                    self.summary = inspection.partition;
                    self.entropy_summary = Some(inspection.entropy);
                    self.partition_error = None;
                }
                Err(error) => {
                    self.summary = None;
                    self.entropy_summary = None;
                    self.partition_error =
                        Some(tr!("result-parse-error", "error" => error.to_string()));
                }
            }
        }
    }

    fn render(&self, ui: &mut egui::Ui, summary: &PartitionSummary) {
        match summary {
            PartitionSummary::Harmony(harmony) => {
                badge_heading(
                    ui,
                    &tr!("harmony-partition-image"),
                    egui::Color32::from_rgb(90, 170, 255),
                );
                self.render_harmony(ui, harmony);
            }
            PartitionSummary::Rvt(rvt) => {
                badge_heading(
                    ui,
                    &tr!("format-rvt-image"),
                    egui::Color32::from_rgb(180, 130, 255),
                );
                self.render_rvt(ui, rvt);
            }
            PartitionSummary::Gpt(gpt) => {
                badge_heading(
                    ui,
                    &tr!("gpt-partition-table"),
                    egui::Color32::from_rgb(100, 200, 140),
                );
                self.render_gpt(ui, gpt);
            }
            PartitionSummary::SecImage(secimg) => {
                badge_heading(
                    ui,
                    &tr!("format-sec-image"),
                    egui::Color32::from_rgb(230, 170, 40),
                );
                self.render_secimg(ui, secimg);
            }
            PartitionSummary::HvbWrapped { footer, cert, .. } => {
                badge_heading(
                    ui,
                    &tr!("hvb-wrapped-partition-image"),
                    egui::Color32::from_rgb(90, 170, 255),
                );
                section(ui, &tr!("hvb-footer"));
                egui::Grid::new("hvb-footer-grid")
                    .num_columns(2)
                    .spacing([18.0, 6.0])
                    .show(ui, |ui| {
                        kv(
                            ui,
                            &tr!("certificate-offset"),
                            crate::util::hex64(footer.cert_offset),
                        );
                        kv(ui, &tr!("certificate-size"), human_size(footer.cert_size));
                        kv(
                            ui,
                            &tr!("image-size"),
                            crate::util::hex64(footer.image_size),
                        );
                        kv(
                            ui,
                            &tr!("partition-size"),
                            crate::util::hex64(footer.partition_size),
                        );
                    });
                ui.add_space(6.0);
                section(ui, &tr!("hvb-certificate"));
                if let Some(cert) = cert {
                    render_cert(ui, cert);
                }
            }
        }
    }

    fn render_harmony(&self, ui: &mut egui::Ui, harmony: &HarmonySummary) {
        section(ui, &tr!("harmony-header"));
        egui::Grid::new("harmony-hdr-grid")
            .num_columns(2)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                kv(
                    ui,
                    &tr!("header-size"),
                    crate::util::hex64(harmony.hdr_size as u64),
                );
                kv(
                    ui,
                    &tr!("image-size"),
                    crate::util::hex64(harmony.image_size as u64),
                );
                kv(ui, &tr!("flags"), crate::util::hex64(harmony.flags as u64));
                kv(ui, &tr!("build-variant"), &harmony.buildvariant);
            });
        ui.add_space(6.0);
        section(ui, &tr!("hvb-footer"));
        egui::Grid::new("harmony-footer-grid")
            .num_columns(2)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                kv(
                    ui,
                    &tr!("certificate-offset"),
                    crate::util::hex64(harmony.footer.cert_offset),
                );
                kv(
                    ui,
                    &tr!("certificate-size"),
                    human_size(harmony.footer.cert_size),
                );
                kv(
                    ui,
                    &tr!("image-size"),
                    crate::util::hex64(harmony.footer.image_size),
                );
                kv(
                    ui,
                    &tr!("partition-size"),
                    crate::util::hex64(harmony.footer.partition_size),
                );
            });
        ui.add_space(6.0);
        section(ui, &tr!("hvb-certificate"));
        render_cert(ui, &harmony.cert);
    }

    fn render_secimg(&self, ui: &mut egui::Ui, secimg: &SecImageInfo) {
        section(ui, &tr!("image-layout"));
        egui::Grid::new("secimg-layout-grid")
            .num_columns(2)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                kv(ui, &tr!("component-name"), &secimg.image_name);
                kv(ui, &tr!("target-partition"), &secimg.partition_name);
                kv(ui, &tr!("file-size"), human_size(secimg.file_size));
                kv(
                    ui,
                    &tr!("certificate-chain-size"),
                    crate::util::hex64(secimg.certificate_chain_size),
                );
                kv(
                    ui,
                    &tr!("payload-offset"),
                    crate::util::hex64(secimg.payload_offset),
                );
                kv(ui, &tr!("payload-size"), human_size(secimg.payload_size));
                if let Some(size) = secimg.secondary_size {
                    kv(ui, &tr!("secondary-declared-size"), human_size(size));
                }
                kv(ui, &tr!("trailing-data"), human_size(secimg.trailing_size));
                kv(
                    ui,
                    &tr!("payload-sha256"),
                    if secimg.payload_hash_valid {
                        tr!("verification-passed")
                    } else {
                        tr!("mismatch")
                    },
                );
            });

        ui.label(
            egui::RichText::new(&secimg.declared_payload_sha256)
                .monospace()
                .small(),
        );
        if !secimg.payload_hash_valid {
            ui.label(
                egui::RichText::new(
                    tr!("actual-value", "value" => secimg.actual_payload_sha256.clone()),
                )
                .monospace()
                .small()
                .color(egui::Color32::from_rgb(230, 90, 90)),
            );
        }

        section(ui, &tr!("x509-certificate-chain"));
        for certificate in &secimg.certificates {
            ui.label(
                egui::RichText::new(format!(
                    "#{}  0x{:X} + 0x{:X}  {}",
                    certificate.chain_index + 1,
                    certificate.offset,
                    certificate.size,
                    certificate.subject
                ))
                .monospace(),
            );
            ui.label(
                egui::RichText::new(tr!(
                    "certificate-validity",
                    "from" => certificate.not_before.clone(),
                    "to" => certificate.not_after.clone(),
                    "algorithm" => certificate.signature_algorithm_oid.clone(),
                ))
                .weak()
                .small(),
            );
        }
    }

    fn render_rvt(&self, ui: &mut egui::Ui, rvt: &common::formats::rvt::RvtInfo) {
        egui::Grid::new("rvt-overview-grid")
            .num_columns(2)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                kv(ui, "verity_num", rvt.verity_num.to_string());
                kv(
                    ui,
                    &tr!("keys-per-partition"),
                    if rvt.raw_key_count == 0 {
                        tr!("legacy-zero-as-one")
                    } else {
                        rvt.raw_key_count.to_string()
                    },
                );
                kv(
                    ui,
                    &tr!("descriptor-count"),
                    rvt.descriptors.len().to_string(),
                );
                kv(
                    ui,
                    &tr!("rvt-content-size"),
                    tr!("byte-count", "count" => rvt.total_size),
                );
                kv(
                    ui,
                    &tr!("wrapper"),
                    if rvt.hvb_wrapped {
                        tr!("hvb-wrapped")
                    } else {
                        tr!("raw-rvt")
                    },
                );
                if let Some(footer) = &rvt.footer {
                    kv(
                        ui,
                        &tr!("partition-size"),
                        crate::util::hex64(footer.partition_size),
                    );
                }
            });
        if let Some(cert) = &rvt.cert {
            ui.add_space(6.0);
            section(ui, &tr!("hvb-certificate"));
            render_cert(ui, &cert_summary(cert));
        }
        if rvt.descriptors.is_empty() {
            return;
        }
        ui.add_space(6.0);
        section(ui, &tr!("partition-public-key-descriptors"));
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::auto().at_least(120.0))
            .column(Column::auto().at_least(130.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(140.0))
            .column(Column::remainder().at_least(140.0))
            .header(24.0, |mut header| {
                header.col(|ui| {
                    ui.strong(tr!("partition"));
                });
                header.col(|ui| {
                    ui.strong(tr!("algorithm"));
                });
                header.col(|ui| {
                    ui.strong(tr!("key-length"));
                });
                header.col(|ui| {
                    ui.strong(tr!("public-key-sha256-short"));
                });
                header.col(|ui| {
                    ui.strong(tr!("backup-key"));
                });
            })
            .body(|mut body| {
                for descriptor in &rvt.descriptors {
                    body.row(24.0, |mut row| {
                        row.col(|ui| {
                            ui.label(&descriptor.name);
                        });
                        row.col(|ui| {
                            ui.label(&descriptor.algorithm);
                        });
                        row.col(|ui| {
                            ui.label(format!("{} B", descriptor.pubkey_len));
                        });
                        row.col(|ui| {
                            let short = descriptor
                                .pubkey_sha256
                                .chars()
                                .take(16)
                                .collect::<String>();
                            ui.label(egui::RichText::new(short).monospace().weak())
                                .on_hover_text(&descriptor.pubkey_sha256);
                        });
                        row.col(|ui| match &descriptor.backup_equals_main {
                            Some(true) => {
                                ui.label(egui::RichText::new(tr!("same-as-main-key")).weak());
                            }
                            Some(false) => {
                                ui.label(
                                    egui::RichText::new(tr!("different"))
                                        .color(egui::Color32::from_rgb(230, 170, 40)),
                                );
                            }
                            None => {
                                ui.label(egui::RichText::new(tr!("none")).weak());
                            }
                        });
                    });
                }
            });
    }

    fn render_gpt(&self, ui: &mut egui::Ui, gpt: &GptInfo) {
        let Some(first_table) = gpt.tables.first() else {
            return;
        };
        let header = &first_table.header;
        section(ui, &tr!("gpt-header"));
        egui::Grid::new("gpt-header-grid")
            .num_columns(2)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                kv(
                    ui,
                    &tr!("version"),
                    format!("{}.{}", header.revision >> 16, header.revision & 0xFFFF),
                );
                kv(ui, &tr!("disk-guid"), &header.disk_guid);
                kv(
                    ui,
                    &tr!("usable-lba-range"),
                    format!(
                        "{} - {}",
                        crate::util::hex64(header.first_usable_lba),
                        crate::util::hex64(header.last_usable_lba)
                    ),
                );
                kv(
                    ui,
                    &tr!("partition-table-entries"),
                    tr!("entries-each-bytes", "count" => header.partition_entry_count, "size" => header.partition_entry_size),
                );
                kv(ui, &tr!("gpt-tables"), gpt.tables.len().to_string());
                kv(ui, &tr!("used-entries"), gpt.partition_count().to_string());
            });

        if gpt.partition_count() == 0 {
            return;
        }

        ui.add_space(6.0);
        section(ui, &tr!("partition"));
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::auto().at_least(38.0))
            .column(Column::auto().at_least(85.0))
            .column(Column::auto().at_least(130.0))
            .column(Column::auto().at_least(105.0))
            .column(Column::auto().at_least(105.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::remainder().at_least(180.0))
            .header(24.0, |mut header| {
                header.col(|ui| {
                    ui.strong(tr!("number"));
                });
                header.col(|ui| {
                    ui.strong(tr!("gpt-table-offset"));
                });
                header.col(|ui| {
                    ui.strong(tr!("partition"));
                });
                header.col(|ui| {
                    ui.strong(tr!("start-lba"));
                });
                header.col(|ui| {
                    ui.strong(tr!("end-lba"));
                });
                header.col(|ui| {
                    ui.strong(tr!("size"));
                });
                header.col(|ui| {
                    ui.strong(tr!("type-guid"));
                });
            })
            .body(|mut body| {
                for table in &gpt.tables {
                    for partition in &table.partitions {
                        body.row(24.0, |mut row| {
                            row.col(|ui| {
                                ui.label(partition.index.to_string());
                            });
                            row.col(|ui| {
                                ui.label(crate::util::hex64(table.image_offset));
                            });
                            row.col(|ui| {
                                ui.label(&partition.name);
                            });
                            row.col(|ui| {
                                ui.label(crate::util::hex64(partition.first_lba));
                            });
                            row.col(|ui| {
                                ui.label(crate::util::hex64(partition.last_lba));
                            });
                            row.col(|ui| {
                                ui.label(human_size(partition.sector_count().saturating_mul(512)));
                            });
                            row.col(|ui| {
                                ui.label(egui::RichText::new(&partition.type_guid).monospace())
                                    .on_hover_text(tr!(
                                        "gpt-partition-tooltip",
                                        "guid" => partition.unique_guid.clone(),
                                        "attributes" => format!("0x{:X}", partition.attributes),
                                        "offset" => format!("0x{:X}", table.entry_array_offset),
                                    ));
                            });
                        });
                    }
                }
            });
    }
}

fn badge_heading(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("●").color(color).strong());
        ui.label(egui::RichText::new(text).strong().size(16.0));
    });
    ui.add_space(4.0);
}

fn render_cert(ui: &mut egui::Ui, cert: &CertSummary) {
    egui::Grid::new("cert-grid")
        .num_columns(2)
        .spacing([18.0, 6.0])
        .show(ui, |ui| {
            kv(
                ui,
                &tr!("version"),
                format!("{}.{}", cert.version_major, cert.version_minor),
            );
            kv(ui, &tr!("partition-name"), &cert.partition_name);
            kv(
                ui,
                &tr!("original-image-length"),
                crate::util::hex64(cert.image_original_len),
            );
            kv(ui, &tr!("image-length"), crate::util::hex64(cert.image_len));
            kv(
                ui,
                &tr!("verity-type"),
                format!(
                    "{} ({})",
                    cert.verity_type,
                    match cert.verity_type {
                        1 => "hash",
                        2 => "hashtree",
                        _ => "?",
                    }
                ),
            );
            kv(
                ui,
                &tr!("hash-algorithm"),
                format!(
                    "{} ({})",
                    cert.hash_algo,
                    match cert.hash_algo {
                        0 => "SHA256",
                        1 => "SHA128",
                        2 => "SHA512",
                        3 => "SM3",
                        _ => "?",
                    }
                ),
            );
            kv(ui, &tr!("salt-length"), cert.salt_size.to_string());
            kv(ui, &tr!("digest-length"), cert.digest_size.to_string());
        });
}

fn cert_summary(cert: &common::formats::hvb::HvbCert) -> CertSummary {
    CertSummary {
        version_major: cert.version_major,
        version_minor: cert.version_minor,
        partition_name: cert.partition_name.clone(),
        image_original_len: cert.image_original_len,
        image_len: cert.image_len,
        verity_type: cert.verity_type,
        hash_algo: cert.hash_algo,
        salt_size: cert.salt_size,
        digest_size: cert.digest_size,
    }
}
