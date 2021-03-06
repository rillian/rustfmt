// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use syntax::ast;
use syntax::codemap::{self, CodeMap, Span, BytePos};
use syntax::visit;

use utils;

use SKIP_ANNOTATION;
use changes::ChangeSet;

pub struct FmtVisitor<'a> {
    pub codemap: &'a CodeMap,
    pub changes: ChangeSet<'a>,
    pub last_pos: BytePos,
    // TODO RAII util for indenting
    pub block_indent: usize,
}

impl<'a, 'v> visit::Visitor<'v> for FmtVisitor<'a> {
    fn visit_expr(&mut self, ex: &'v ast::Expr) {
        debug!("visit_expr: {:?} {:?}",
               self.codemap.lookup_char_pos(ex.span.lo),
               self.codemap.lookup_char_pos(ex.span.hi));
        self.format_missing(ex.span.lo);
        let offset = self.changes.cur_offset_span(ex.span);
        let new_str = self.rewrite_expr(ex, config!(max_width) - offset, offset);
        self.changes.push_str_span(ex.span, &new_str);
        self.last_pos = ex.span.hi;
    }

    fn visit_stmt(&mut self, stmt: &'v ast::Stmt) {
        // If the stmt is actually an item, then we'll handle any missing spans
        // there. This is important because of annotations.
        // Although it might make more sense for the statement span to include
        // any annotations on the item.
        let skip_missing = match stmt.node {
            ast::Stmt_::StmtDecl(ref decl, _) => {
                match decl.node {
                    ast::Decl_::DeclItem(_) => true,
                    _ => false,
                }
            }
            _ => false,
        };
        if !skip_missing {
            self.format_missing_with_indent(stmt.span.lo);
        }
        visit::walk_stmt(self, stmt);
    }

    fn visit_block(&mut self, b: &'v ast::Block) {
        debug!("visit_block: {:?} {:?}",
               self.codemap.lookup_char_pos(b.span.lo),
               self.codemap.lookup_char_pos(b.span.hi));
        self.format_missing(b.span.lo);

        self.changes.push_str_span(b.span, "{");
        self.last_pos = self.last_pos + BytePos(1);
        self.block_indent += config!(tab_spaces);

        for stmt in &b.stmts {
            self.visit_stmt(&stmt)
        }
        match b.expr {
            Some(ref e) => {
                self.format_missing_with_indent(e.span.lo);
                self.visit_expr(e);
            }
            None => {}
        }

        self.block_indent -= config!(tab_spaces);
        // TODO we should compress any newlines here to just one
        self.format_missing_with_indent(b.span.hi - BytePos(1));
        self.changes.push_str_span(b.span, "}");
        self.last_pos = b.span.hi;
    }

    // Note that this only gets called for function definitions. Required methods
    // on traits do not get handled here.
    fn visit_fn(&mut self,
                fk: visit::FnKind<'v>,
                fd: &'v ast::FnDecl,
                b: &'v ast::Block,
                s: Span,
                _: ast::NodeId) {
        self.format_missing_with_indent(s.lo);
        self.last_pos = s.lo;

        let indent = self.block_indent;
        match fk {
            visit::FkItemFn(ident,
                            ref generics,
                            ref unsafety,
                            ref constness,
                            ref abi,
                            vis) => {
                let new_fn = self.rewrite_fn(indent,
                                             ident,
                                             fd,
                                             None,
                                             generics,
                                             unsafety,
                                             constness,
                                             abi,
                                             vis,
                                             b.span.lo);
                self.changes.push_str_span(s, &new_fn);
            }
            visit::FkMethod(ident, ref sig, vis) => {
                let new_fn = self.rewrite_fn(indent,
                                             ident,
                                             fd,
                                             Some(&sig.explicit_self),
                                             &sig.generics,
                                             &sig.unsafety,
                                             &sig.constness,
                                             &sig.abi,
                                             vis.unwrap_or(ast::Visibility::Inherited),
                                             b.span.lo);
                self.changes.push_str_span(s, &new_fn);
            }
            visit::FkFnBlock(..) => {}
        }

        self.last_pos = b.span.lo;
        self.visit_block(b)
    }

    fn visit_item(&mut self, item: &'v ast::Item) {
        // Don't look at attributes for modules.
        // We want to avoid looking at attributes in another file, which the AST
        // doesn't distinguish. FIXME This is overly conservative and means we miss
        // attributes on inline modules.
        match item.node {
            ast::Item_::ItemMod(_) => {}
            _ => {
                if self.visit_attrs(&item.attrs) {
                    return;
                }
            }
        }

        match item.node {
            ast::Item_::ItemUse(ref vp) => {
                self.format_missing_with_indent(item.span.lo);
                match vp.node {
                    ast::ViewPath_::ViewPathList(ref path, ref path_list) => {
                        let block_indent = self.block_indent;
                        let one_line_budget = config!(max_width) - block_indent;
                        let multi_line_budget = config!(ideal_width) - block_indent;
                        let new_str = self.rewrite_use_list(block_indent,
                                                            one_line_budget,
                                                            multi_line_budget,
                                                            path,
                                                            path_list,
                                                            item.vis);
                        self.changes.push_str_span(item.span, &new_str);
                        self.last_pos = item.span.hi;
                    }
                    ast::ViewPath_::ViewPathGlob(_) => {
                        // FIXME convert to list?
                    }
                    ast::ViewPath_::ViewPathSimple(_,_) => {}
                }
                visit::walk_item(self, item);
            }
            ast::Item_::ItemImpl(..) |
            ast::Item_::ItemMod(_) |
            ast::Item_::ItemTrait(..) => {
                self.block_indent += config!(tab_spaces);
                visit::walk_item(self, item);
                self.block_indent -= config!(tab_spaces);
            }
            ast::Item_::ItemExternCrate(_) => {
                self.format_missing_with_indent(item.span.lo);
                let new_str = self.snippet(item.span);
                self.changes.push_str_span(item.span, &new_str);
                self.last_pos = item.span.hi;
            }
            ast::Item_::ItemStruct(ref def, ref generics) => {
                self.format_missing_with_indent(item.span.lo);
                self.visit_struct(item.ident,
                                  item.vis,
                                  def,
                                  generics,
                                  item.span);
                self.last_pos = item.span.hi;
            }
            _ => {
                visit::walk_item(self, item);
            }
        }
    }

    fn visit_trait_item(&mut self, ti: &'v ast::TraitItem) {
        if self.visit_attrs(&ti.attrs) {
            return;
        }

        if let ast::TraitItem_::MethodTraitItem(ref sig, None) = ti.node {
            self.format_missing_with_indent(ti.span.lo);

            let indent = self.block_indent;
            let new_fn = self.rewrite_required_fn(indent,
                                                  ti.ident,
                                                  sig,
                                                  ti.span);

            self.changes.push_str_span(ti.span, &new_fn);
            self.last_pos = ti.span.hi;
        }
        // TODO format trait types

        visit::walk_trait_item(self, ti)
    }

    fn visit_impl_item(&mut self, ii: &'v ast::ImplItem) {
        if self.visit_attrs(&ii.attrs) {
            return;
        }
        visit::walk_impl_item(self, ii)
    }

    fn visit_mac(&mut self, mac: &'v ast::Mac) {
        visit::walk_mac(self, mac)
    }

    fn visit_mod(&mut self, m: &'v ast::Mod, s: Span, _: ast::NodeId) {
        // Only visit inline mods here.
        if self.codemap.lookup_char_pos(s.lo).file.name !=
           self.codemap.lookup_char_pos(m.inner.lo).file.name {
            return;
        }
        visit::walk_mod(self, m);
    }
}

impl<'a> FmtVisitor<'a> {
    pub fn from_codemap<'b>(codemap: &'b CodeMap) -> FmtVisitor<'b> {
        FmtVisitor {
            codemap: codemap,
            changes: ChangeSet::from_codemap(codemap),
            last_pos: BytePos(0),
            block_indent: 0,
        }
    }

    pub fn snippet(&self, span: Span) -> String {
        match self.codemap.span_to_snippet(span) {
            Ok(s) => s,
            Err(_) => {
                println!("Couldn't make snippet for span {:?}->{:?}",
                         self.codemap.lookup_char_pos(span.lo),
                         self.codemap.lookup_char_pos(span.hi));
                "".to_owned()
            }
        }
    }

    // Returns true if we should skip the following item.
    pub fn visit_attrs(&mut self, attrs: &[ast::Attribute]) -> bool {
        if attrs.len() == 0 {
            return false;
        }

        let first = &attrs[0];
        self.format_missing_with_indent(first.span.lo);

        match self.rewrite_attrs(attrs, self.block_indent) {
            Some(s) => {
                self.changes.push_str_span(first.span, &s);
                let last = attrs.last().unwrap();
                self.last_pos = last.span.hi;
                false
            }
            None => true
        }
    }

    fn rewrite_attrs(&self, attrs: &[ast::Attribute], indent: usize) -> Option<String> {
        let mut result = String::new();
        let indent = utils::make_indent(indent);

        for (i, a) in attrs.iter().enumerate() {
            if is_skip(&a.node.value) {
                return None;
            }

            let a_str = self.snippet(a.span);

            if i > 0 {
                let comment = self.snippet(codemap::mk_sp(attrs[i-1].span.hi, a.span.lo));
                // This particular horror show is to preserve line breaks in between doc
                // comments. An alternative would be to force such line breaks to start
                // with the usual doc comment token.
                let multi_line = a_str.starts_with("//") && comment.matches('\n').count() > 1;
                let comment = comment.trim();
                if comment.len() > 0 {
                    result.push_str(&indent);
                    result.push_str(comment);
                    result.push('\n');
                } else if multi_line {
                    result.push('\n');
                }
                result.push_str(&indent);
            }

            result.push_str(&a_str);

            if i < attrs.len() -1 {
                result.push('\n');
            }
        }

        Some(result)
    }
}

fn is_skip(meta_item: &ast::MetaItem) -> bool {
    match meta_item.node {
        ast::MetaItem_::MetaWord(ref s) => *s == SKIP_ANNOTATION,
        _ => false,
    }
}
