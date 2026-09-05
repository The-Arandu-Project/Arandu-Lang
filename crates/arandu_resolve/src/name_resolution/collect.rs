use arandu_parser::{FuncName, ImportDecl, TopLevelDecl, Visibility};

use crate::{DiagCode, Diagnostic, ScopeId, SymbolKind};

use super::Resolver;
use super::util::is_type_case;

#[inline]
fn is_public(vis: Visibility) -> bool {
    matches!(vis, Visibility::Public)
}

impl<'a> Resolver<'a> {
    pub(crate) fn collect_import(&mut self, scope: ScopeId, import: &ImportDecl) {
        match import {
            ImportDecl::ModuleAlias { span, alias, .. } => {
                // Import aliases are file-local (never re-exported via this name).
                if let Some(sym) = self.define(scope, alias, SymbolKind::Module, *span) {
                    // SmolStr::clone is O(1)
                    self.record_import_symbol(sym, alias.clone(), *span);
                }
            }
            ImportDecl::Named { items, .. } => {
                for item in items {
                    let name = item.alias.as_ref().unwrap_or(&item.name);
                    let kind = if is_type_case(name) {
                        SymbolKind::ImportType
                    } else {
                        SymbolKind::ImportValue
                    };
                    if let Some(sym) = self.define(scope, name, kind, item.span) {
                        // SmolStr::clone is O(1)
                        self.record_import_symbol(sym, name.clone(), item.span);
                    }
                }
            }
            ImportDecl::ExternalAlias {
                span,
                source,
                alias,
            } => {
                if let Some(sym) = self.define(scope, alias, SymbolKind::Module, *span) {
                    // SmolStr::clone is O(1)
                    self.record_import_symbol(sym, alias.clone(), *span);
                }
                // SmolStr::clone is O(1)
                self.import_aliases.insert(alias.clone(), source.clone());
            }
            ImportDecl::ExternalNamed { items, .. } => {
                for item in items {
                    let name = item.alias.as_ref().unwrap_or(&item.name);
                    let kind = if is_type_case(name) {
                        SymbolKind::ImportType
                    } else {
                        SymbolKind::ImportValue
                    };
                    if let Some(sym) = self.define(scope, name, kind, item.span) {
                        self.record_import_symbol(sym, name.clone(), item.span);
                    }
                }
            }
        }
    }

    pub(crate) fn collect_top_level(&mut self, scope: ScopeId, decl: &TopLevelDecl) {
        match decl {
            TopLevelDecl::Const(decl) => {
                self.define_vis(
                    scope,
                    &decl.name,
                    SymbolKind::Const,
                    decl.span,
                    is_public(decl.visibility),
                );
            }
            TopLevelDecl::TypeAlias(decl) => {
                self.define_vis(
                    scope,
                    &decl.name,
                    SymbolKind::TypeAlias,
                    decl.span,
                    is_public(decl.visibility),
                );
            }
            TopLevelDecl::Func(decl) => match &decl.name {
                FuncName::Free { span, name } => {
                    self.define_vis(
                        scope,
                        name,
                        SymbolKind::Func,
                        *span,
                        is_public(decl.visibility),
                    );
                }
                FuncName::Method {
                    span,
                    receiver,
                    name,
                } => {
                    let receiver_str = receiver.path.join(".");
                    let method_name = format!("{receiver_str}.{name}");
                    let global = self.symbols.global_scope();
                    match self.symbols.define_vis(
                        global,
                        &method_name,
                        SymbolKind::AssociatedFunc,
                        *span,
                        is_public(decl.visibility),
                    ) {
                        Ok(symbol) => {
                            self.resolved.define(*span, symbol);
                            if let Some(type_sym) = self.symbols.lookup_type(global, &receiver_str)
                            {
                                self.symbols
                                    .associated_members
                                    .insert((type_sym, name.clone()), symbol);
                            }
                        }
                        Err(previous) => {
                            let previous_symbol = self.symbols.get(previous);
                            self.diagnostics.push(
                                Diagnostic::error(
                                    DiagCode::N003RedefinedName,
                                    format!(
                                        "associated function '{receiver_str}.{name}' is already declared"
                                    ),
                                    *span,
                                )
                                .with_label(previous_symbol.span, "previous declaration is here"),
                            );
                        }
                    }
                }
            },
            TopLevelDecl::Struct(decl) => {
                self.define_vis(
                    scope,
                    &decl.name,
                    SymbolKind::Struct,
                    decl.span,
                    is_public(decl.visibility),
                );
            }
            TopLevelDecl::Enum(decl) => {
                let pub_ = is_public(decl.visibility);
                if let Some(enum_sym) =
                    self.define_vis(scope, &decl.name, SymbolKind::Enum, decl.span, pub_)
                {
                    match (self.current_module.as_deref(), decl.name.as_str()) {
                        (Some("std.core.future"), "Poll") => {
                            self.symbols.set_lang_item(
                                enum_sym,
                                arandu_middle::symbol_table::LangItem::Poll,
                            );
                        }
                        (Some("std.core.result"), "Result") => {
                            self.symbols.set_lang_item(
                                enum_sym,
                                arandu_middle::symbol_table::LangItem::Result,
                            );
                        }
                        (Some("std.core.option"), "Option") => {
                            self.symbols.set_lang_item(
                                enum_sym,
                                arandu_middle::symbol_table::LangItem::Option,
                            );
                        }
                        (Some("std.core.coroutine"), "Coroutine") => {
                            self.symbols.set_lang_item(
                                enum_sym,
                                arandu_middle::symbol_table::LangItem::Coroutine,
                            );
                        }
                        _ => {}
                    }
                    // Variants inherit the enum's export visibility (public enum → public ctors).
                    for variant in &decl.variants {
                        if let Ok(symbol) = self.symbols.define_associated_member_vis(
                            enum_sym,
                            &variant.name,
                            variant.span,
                            pub_,
                        ) {
                            self.resolved.define(variant.span, symbol);
                            match (
                                self.current_module.as_deref(),
                                decl.name.as_str(),
                                variant.name.as_str(),
                            ) {
                                (Some("std.core.option"), "Option", "Some") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::OptionSome,
                                    );
                                }
                                (Some("std.core.option"), "Option", "None") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::OptionNone,
                                    );
                                }
                                (Some("std.core.result"), "Result", "Ok") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::ResultOk,
                                    );
                                }
                                (Some("std.core.result"), "Result", "Err") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::ResultErr,
                                    );
                                }
                                (Some("std.core.future"), "Poll", "Ready") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::PollReady,
                                    );
                                }
                                (Some("std.core.future"), "Poll", "Pending") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::PollPending,
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            TopLevelDecl::Interface(decl) => {
                self.define_vis(
                    scope,
                    &decl.name,
                    SymbolKind::Interface,
                    decl.span,
                    is_public(decl.visibility),
                );
            }
            TopLevelDecl::Extern(decl) => {
                // Intrinsics / FFI block members are the module surface (exportable).
                for member in &decl.members {
                    self.define_vis(
                        scope,
                        &member.name,
                        SymbolKind::ExternFunc,
                        member.span,
                        true,
                    );
                }
            }
            TopLevelDecl::Error(_) => {}
        }
    }
}
