//! XML qualified names — `QName`, `NcName`, namespace types — for the data model.
//!
//! This is a self-contained, lock-free reimplementation of the small slice of the
//! upstream `qualname` 0.0.1 crate that chadpath uses. It is **API- and
//! semantics-compatible** with that crate: the public types, methods, validation
//! rules (NCName / normalized-value grammar), equality, ordering, hashing, and
//! `Display` all behave identically. Only the *internals* of the interned string
//! type changed.
//!
//! ### Why it was vendored
//! Upstream backed every name (`NcName`, `NamespaceUri`) with a `UniqueString`
//! that was an index into one **process-global `RwLock<Vec<…>>` arena**. Cloning
//! or dropping a name took the global **write** lock, and constructing one took
//! the write lock *plus a linear scan*. chadpath resolves a node's name on every
//! name test, so a `//tag` sweep over an N-node document took N global write-lock
//! round-trips — uncontended single-threaded, but a hard serialization point that
//! pegged the multi-threaded crawler fleet (the work showed up as kernel CPU:
//! futex traffic and context switches, with zero useful throughput gained from
//! adding threads).
//!
//! ### What changed
//! `UniqueString` now holds an `Arc<str>`. Clone/drop are lock-free atomic
//! refcount operations; construction is a single allocation; equality is
//! content-based with an `Arc::ptr_eq` fast path; hashing is over the bytes. There
//! is no shared mutable state, so there is no global lock anywhere on the path.
//! `Arc` (not `Rc`) keeps the types `Send + Sync`, which the namespace statics and
//! the `Node` trait's name API require.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════════
//  UniqueString — the interned-by-value string backing every name
// ═══════════════════════════════════════════════════════════════════════════

/// An owned, cheaply-clonable string used as the storage for names. Clones share
/// the same allocation (atomic refcount); two `UniqueString`s are equal when
/// their contents are equal (with a pointer-identity fast path).
#[derive(Clone, Debug)]
pub(crate) struct UniqueString(Arc<str>);

impl UniqueString {
    pub fn empty() -> Self {
        UniqueString(Arc::from(""))
    }
    pub fn to_string(&self) -> String {
        self.0.as_ref().to_owned()
    }
    /// Borrow the contents without allocating.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for UniqueString {
    fn from(value: &str) -> Self {
        UniqueString(Arc::from(value))
    }
}

impl PartialEq for UniqueString {
    fn eq(&self, other: &Self) -> bool {
        // Fast path: clones of the same name share an allocation. Fall back to a
        // content comparison so independently-constructed equal names compare
        // equal (the behaviour the old interner guaranteed by construction).
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}
impl Eq for UniqueString {}

impl Hash for UniqueString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash the contents (not the pointer), so equal strings hash equally —
        // required for use as a HashMap key and consistent with `PartialEq`.
        self.0.as_ref().hash(state);
    }
}

impl PartialEq<String> for UniqueString {
    fn eq(&self, other: &String) -> bool {
        self.0.as_ref() == other.as_str()
    }
}
impl PartialEq<str> for UniqueString {
    fn eq(&self, other: &str) -> bool {
        self.0.as_ref() == other
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  NcName / NamespaceUri / NamespacePrefix and the XML name grammar
// ═══════════════════════════════════════════════════════════════════════════

/// An invalid Qualified Name.
#[derive(Debug)]
pub struct InvalidQName;
impl Display for InvalidQName {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "InvalidQName")
    }
}
impl std::error::Error for InvalidQName {}

/// A grammatically valid Namespace URI.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NamespaceUri(pub(crate) UniqueString);
impl NamespaceUri {
    #[must_use]
    pub fn empty() -> Self {
        Self(UniqueString::empty())
    }
    pub fn to_string(&self) -> String {
        self.0.to_string()
    }
}
impl TryFrom<&str> for NamespaceUri {
    type Error = InvalidQName;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if check_normalized_value(value) {
            Ok(Self(UniqueString::from(value)))
        } else {
            Err(InvalidQName)
        }
    }
}
impl PartialEq<String> for NamespaceUri {
    fn eq(&self, other: &String) -> bool {
        self.0.to_string().eq(other)
    }
}
impl PartialEq<NamespaceUri> for String {
    fn eq(&self, other: &NamespaceUri) -> bool {
        self.eq(&other.to_string())
    }
}
impl PartialEq<str> for NamespaceUri {
    fn eq(&self, other: &str) -> bool {
        self.0.to_string().eq(other)
    }
}
impl PartialEq<NamespaceUri> for str {
    fn eq(&self, other: &NamespaceUri) -> bool {
        self.eq(&other.0.to_string())
    }
}

// NameStartChar without ':' — https://www.w3.org/TR/REC-xml/#d0e804
fn is_ncname_start_char(c: char) -> bool {
    c.is_ascii_alphabetic()
        || c == '_'
        || (c >= 0xC0 as char && c <= '\u{2FF}' && c != 0xD7 as char && c != 0xF7 as char)
        || (('\u{370}'..='\u{1FFF}').contains(&c) && c != '\u{37E}')
        || ('\u{200C}'..='\u{200D}').contains(&c)
        || ('\u{2070}'..='\u{218F}').contains(&c)
        || ('\u{2C00}'..='\u{2FEF}').contains(&c)
        || ('\u{3001}'..='\u{D7FF}').contains(&c)
        || ('\u{F900}'..='\u{FDCF}').contains(&c)
        || ('\u{FDF0}'..='\u{FFFD}').contains(&c)
        || ('\u{10000}'..='\u{EFFFF}').contains(&c)
}

// NameChar without ':' — https://www.w3.org/TR/REC-xml/#d0e804
fn is_ncname_char(c: char) -> bool {
    is_ncname_start_char(c)
        || c == '-'
        || c == '.'
        || c.is_ascii_digit()
        || c == 0xb7 as char
        || ('\u{300}'..='\u{36F}').contains(&c)
        || c == '\u{203F}'
        || c == '\u{2040}'
}

fn parse_ncname(input: &str) -> Result<(&str, &str), ()> {
    let mut chars = input.char_indices();
    if let Some((_, c)) = chars.next() {
        if !is_ncname_start_char(c) {
            return Err(());
        }
    } else {
        return Err(());
    }
    for (end, c) in chars {
        if !is_ncname_char(c) {
            return Ok((&input[..end], &input[end..]));
        }
    }
    Ok((input, ""))
}

// A normalized attribute value: space is the only allowed whitespace, with no
// leading/trailing/double spaces. https://www.w3.org/TR/REC-xml/#AVNormalize
fn check_normalized_value(input: &str) -> bool {
    if input.starts_with(' ') || input.contains("  ") || input.ends_with(' ') {
        return false;
    }
    if input.bytes().any(|b| b == b'\t' || b == b'\r' || b == b'\n') {
        return false;
    }
    true
}

/// A grammatically valid `NcName`.
#[derive(Clone, Debug, Eq, Hash)]
pub struct NcName(pub(crate) UniqueString);
impl NcName {
    pub fn to_string(&self) -> String {
        self.0.to_string()
    }
    /// Borrow the local name without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl TryFrom<&str> for NcName {
    type Error = InvalidQName;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if let Ok((ncname, left)) = parse_ncname(value) {
            if left.is_empty() {
                return Ok(Self(ncname.into()));
            }
        }
        Err(InvalidQName)
    }
}
impl PartialEq<NcName> for NcName {
    fn eq(&self, other: &NcName) -> bool {
        self.0.eq(&other.0)
    }
}
impl PartialEq<String> for NcName {
    fn eq(&self, other: &String) -> bool {
        self.0.to_string().eq(other)
    }
}
impl PartialEq<NcName> for String {
    fn eq(&self, other: &NcName) -> bool {
        self.eq(&other.0.to_string())
    }
}
impl PartialEq<str> for NcName {
    fn eq(&self, other: &str) -> bool {
        self.0.eq(other)
    }
}
impl PartialEq<NcName> for str {
    fn eq(&self, other: &NcName) -> bool {
        other.0.eq(self)
    }
}
impl PartialOrd for NcName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

/// A grammatically valid namespace prefix.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamespacePrefix(pub(crate) String);
impl NamespacePrefix {
    #[must_use]
    pub fn empty() -> Self {
        Self(String::new())
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn to_string(&self) -> String {
        self.0.clone()
    }
    /// Returns the prefix as an `NcName` (the prefix is guaranteed to be one).
    pub fn to_ncname(&self) -> NcName {
        NcName::try_from(self.0.as_str()).unwrap()
    }
}
impl TryFrom<&str> for NamespacePrefix {
    type Error = InvalidQName;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Ok(Self::empty());
        }
        if value == "xmlns" {
            return Err(InvalidQName);
        }
        if let Ok((ncname, left)) = parse_ncname(value) {
            if left.is_empty() {
                return Ok(Self(ncname.into()));
            }
        }
        Err(InvalidQName)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  UniqueQName — namespace-URI-qualified name, used for QName identity
// ═══════════════════════════════════════════════════════════════════════════

/// A namespace-qualified name. Equality and hashing use the URI-qualified form
/// (`Q{uri}local`), so two names are equal iff both URI and local part match.
#[derive(Clone, Debug)]
pub(crate) struct UniqueQName {
    namespace_uri: Option<NamespaceUri>,
    local_name: NcName,
    uriqualified: UniqueString,
}
impl UniqueQName {
    pub fn new(namespace_uri: Option<NamespaceUri>, local_name: NcName) -> Self {
        let uriqualified = format!(
            "{}{}",
            namespace_uri.as_ref().map_or_else(String::new, |ns| {
                let mut result = String::from("Q");
                result.push('{');
                result.push_str(ns.to_string().as_str());
                result.push('}');
                result
            }),
            local_name.to_string()
        );
        let qual_uniq = UniqueString::from(uriqualified.as_str());
        UniqueQName {
            namespace_uri,
            local_name,
            uriqualified: qual_uniq,
        }
    }
    pub fn namespace_uri(&self) -> Option<NamespaceUri> {
        self.namespace_uri.clone()
    }
    pub fn local_name(&self) -> NcName {
        self.local_name.clone()
    }
}
impl PartialEq for UniqueQName {
    fn eq(&self, other: &Self) -> bool {
        self.uriqualified.eq(&other.uriqualified)
    }
}
impl Eq for UniqueQName {}
impl Hash for UniqueQName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.uriqualified.hash(state)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  QName
// ═══════════════════════════════════════════════════════════════════════════

/// A Qualified Name in XML Namespaces. Cloning is cheap (shared name storage)
/// and comparing is content-based on the URI-qualified form.
#[derive(Clone, Debug)]
pub struct QName {
    qname: UniqueQName,
}
impl QName {
    /// Create a Qualified Name using a [`NamespaceDeclaration`].
    #[must_use]
    pub fn new(local_name: NcName, namespace: Option<NamespaceDeclaration>) -> Self {
        Self {
            qname: UniqueQName::new(namespace.map(|nd| nd.namespace_uri), local_name),
        }
    }
    /// Create a Qualified Name using a [`NamespaceUri`].
    #[must_use]
    pub fn new_from_parts(local_name: NcName, namespace_uri: Option<NamespaceUri>) -> Self {
        Self {
            qname: UniqueQName::new(namespace_uri, local_name),
        }
    }
    /// Create an unprefixed Qualified Name (no namespace URI).
    #[must_use]
    pub fn from_local_name(local_name: NcName) -> Self {
        Self {
            qname: UniqueQName::new(None, local_name),
        }
    }
    /// The local part of the Qualified Name.
    #[must_use]
    pub fn local_name(&self) -> NcName {
        self.qname.local_name()
    }
    /// The [`NamespaceUri`] of the Qualified Name, if any.
    #[must_use]
    pub fn namespace_uri(&self) -> Option<NamespaceUri> {
        self.qname.namespace_uri()
    }
}
/// Creates an unprefixed QName. Value must be a valid NcName.
impl TryFrom<&str> for QName {
    type Error = InvalidQName;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Ok(QName {
            qname: UniqueQName::new(None, NcName::try_from(value)?),
        })
    }
}
impl PartialEq for QName {
    fn eq(&self, other: &Self) -> bool {
        PartialEq::eq(&self.qname, &other.qname)
    }
}
impl Eq for QName {}
impl PartialOrd for QName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.local_name().partial_cmp(&other.local_name()).map(|p| {
            if p == Ordering::Equal {
                match (self.namespace_uri(), other.namespace_uri()) {
                    (None, None) => Ordering::Equal,
                    (Some(sns), Some(ons)) => sns
                        .to_string()
                        .partial_cmp(&ons.to_string())
                        .map_or(Ordering::Equal, |q| q),
                    _ => Ordering::Less,
                }
            } else {
                p
            }
        })
    }
}
impl Ord for QName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).map_or(Ordering::Equal, |p| p)
    }
}
impl Hash for QName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.qname.hash(state);
    }
}
impl Display for QName {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let nsuri = self
            .namespace_uri()
            .map_or(String::new(), |ns| format!("{{{}}}", ns.to_string()));
        write!(f, "{}{}", nsuri, self.local_name().to_string())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  NamespaceDeclaration / NamespaceMap
// ═══════════════════════════════════════════════════════════════════════════

static XMLNSURI: std::sync::LazyLock<NamespaceUri> =
    std::sync::LazyLock::new(|| NamespaceUri::try_from("http://www.w3.org/2000/xmlns/").unwrap());
static XMLNSPREFIX: std::sync::LazyLock<Option<NamespacePrefix>> =
    std::sync::LazyLock::new(|| Some(NamespacePrefix(String::from("xmlns"))));
static XML: std::sync::LazyLock<NamespaceDeclaration> =
    std::sync::LazyLock::new(|| NamespaceDeclaration {
        namespace_prefix: Some(NamespacePrefix::try_from("xml").unwrap()),
        namespace_uri: NamespaceUri::try_from("http://www.w3.org/XML/1998/namespace").unwrap(),
    });

/// An invalid namespace declaration.
#[derive(Debug)]
pub struct InvalidNamespaceDeclaration;
impl Display for InvalidNamespaceDeclaration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid namespace declaration")
    }
}
impl std::error::Error for InvalidNamespaceDeclaration {}

/// An optional [`NamespacePrefix`] (None for the default namespace) bound to a
/// [`NamespaceUri`].
#[derive(Clone, Debug)]
pub struct NamespaceDeclaration {
    pub(crate) namespace_prefix: Option<NamespacePrefix>,
    pub(crate) namespace_uri: NamespaceUri,
}
impl NamespaceDeclaration {
    /// # Errors
    /// If the `xml`/`xmlns` prefixes are bound to namespaces other than their
    /// predefined ones (or vice versa), an error is returned.
    pub fn new(
        namespace_prefix: Option<NamespacePrefix>,
        namespace_uri: NamespaceUri,
    ) -> Result<Self, InvalidNamespaceDeclaration> {
        if (namespace_prefix == XML.namespace_prefix && namespace_uri != XML.namespace_uri)
            || (namespace_prefix != XML.namespace_prefix && namespace_uri == XML.namespace_uri)
        {
            return Err(InvalidNamespaceDeclaration);
        }
        if (namespace_prefix == *XMLNSPREFIX && namespace_uri != *XMLNSURI)
            || (namespace_prefix != *XMLNSPREFIX && namespace_uri == *XMLNSURI)
        {
            return Err(InvalidNamespaceDeclaration);
        }
        Ok(Self {
            namespace_prefix,
            namespace_uri,
        })
    }
}

/// Looks up a [`NamespaceDeclaration`] by prefix or by Namespace URI — used to
/// track in-scope namespaces.
#[derive(Clone, Debug)]
pub struct NamespaceMap {
    prefixes: HashMap<Option<NamespacePrefix>, Rc<NamespaceDeclaration>>,
    uris: HashMap<NamespaceUri, Rc<NamespaceDeclaration>>,
}
impl Default for NamespaceMap {
    fn default() -> Self {
        Self::new()
    }
}
impl NamespaceMap {
    pub fn new() -> Self {
        let mut nm = Self {
            prefixes: HashMap::new(),
            uris: HashMap::new(),
        };
        nm.push(
            NamespaceDeclaration::new(
                Some(NamespacePrefix::try_from("xml").unwrap()),
                NamespaceUri::try_from("http://www.w3.org/XML/1998/namespace").unwrap(),
            )
            .unwrap(),
        );
        nm
    }
    pub fn push(&mut self, nsd: NamespaceDeclaration) {
        let nsdr = Rc::new(nsd.clone());
        self.prefixes
            .insert(nsd.namespace_prefix.clone(), nsdr.clone());
        self.uris.insert(nsd.namespace_uri.clone(), nsdr);
    }
    pub fn namespace_uri(&self, prefix: &Option<NamespacePrefix>) -> Option<NamespaceUri> {
        self.prefixes.get(prefix).map(|nd| nd.namespace_uri.clone())
    }
    pub fn prefix(&self, ns_uri: &NamespaceUri) -> Option<NamespacePrefix> {
        self.uris
            .get(ns_uri)
            .map_or(None, |nd| nd.namespace_prefix.clone())
    }
    /// Remove a namespace by prefix, returning the removed declaration if any.
    pub fn pop_prefix(&mut self, prefix: &Option<NamespacePrefix>) -> Option<NamespaceDeclaration> {
        if let Some(nsd) = self.prefixes.remove(prefix) {
            self.uris.remove(&nsd.namespace_uri);
            Some((*nsd).clone())
        } else {
            None
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  QNameCollection
// ═══════════════════════════════════════════════════════════════════════════

/// An index into a [`QNameCollection`].
#[derive(Clone, Copy)]
pub struct QNameCollectionIndex {
    global_index: u32,
    local_index: u32,
}
impl PartialEq for QNameCollectionIndex {
    fn eq(&self, other: &Self) -> bool {
        self.global_index == other.global_index
    }
}
impl Eq for QNameCollectionIndex {}
impl PartialEq<QName> for QNameCollectionIndex {
    fn eq(&self, _other: &QName) -> bool {
        false
    }
}
impl PartialEq<QNameCollectionIndex> for QName {
    fn eq(&self, _other: &QNameCollectionIndex) -> bool {
        false
    }
}

/// A collection of Qualified Names.
pub struct QNameCollection {
    qnames: Vec<QName>,
}
impl Default for QNameCollection {
    fn default() -> Self {
        Self::new()
    }
}
impl QNameCollection {
    #[must_use]
    pub fn new() -> Self {
        Self { qnames: Vec::new() }
    }
    fn find(
        &self,
        namespace: &NamespaceDeclaration,
        local_name: &NcName,
    ) -> Option<QNameCollectionIndex> {
        if let Some((local_index, _qname)) = self.qnames.iter().enumerate().find(|(_, qname)| {
            qname.local_name() == *local_name
                && qname
                    .namespace_uri()
                    .as_ref()
                    .map_or("".to_string(), |ns| ns.to_string())
                    == namespace.namespace_uri.to_string()
        }) {
            return Some(QNameCollectionIndex {
                global_index: 0,
                local_index: u32::try_from(local_index).unwrap_or_else(|_| unreachable!()),
            });
        }
        None
    }
    /// # Panics
    /// If there are more than 2^32 different `QNames`.
    pub fn add(
        &mut self,
        namespace: NamespaceDeclaration,
        local_name: NcName,
    ) -> QNameCollectionIndex {
        if let Some(index) = self.find(&namespace, &local_name) {
            return index;
        }
        let qname = QName::new(local_name, Some(namespace));
        let index = QNameCollectionIndex {
            global_index: 0,
            local_index: u32::try_from(self.qnames.len())
                .expect("Too many QNames in the QNameCollection"),
        };
        self.qnames.push(qname);
        index
    }
    #[must_use]
    pub fn get(&self, index: QNameCollectionIndex) -> &QName {
        &self.qnames[index.local_index as usize]
    }
    #[must_use]
    pub fn local_name(&self, index: QNameCollectionIndex) -> NcName {
        self.get(index).local_name()
    }
    #[must_use]
    pub fn namespace_uri(&self, index: QNameCollectionIndex) -> Option<NamespaceUri> {
        self.get(index).namespace_uri()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests — name grammar + equality/identity semantics
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_ncname() {
        let (ncname, left) = parse_ncname("ok").unwrap();
        assert_eq!(ncname, "ok");
        assert_eq!(left, "");
    }

    #[test]
    fn parse_valid_ncname_with_extra() {
        let (ncname, left) = parse_ncname("ok!").unwrap();
        assert_eq!(ncname, "ok");
        assert_eq!(left, "!");
    }

    #[test]
    fn parse_invalid_ncname() {
        assert!(parse_ncname("!").is_err());
        assert!(parse_ncname(":").is_err());
        assert!(parse_ncname(" ").is_err());
    }

    #[test]
    fn invalid_xmlns_prefix() {
        assert!(NamespacePrefix::try_from("xmlns").is_err());
    }

    #[test]
    fn normalized_attribute_value() {
        assert!(check_normalized_value(""));
        assert!(check_normalized_value("ok"));
        assert!(!check_normalized_value(" "));
        assert!(!check_normalized_value(" a"));
        assert!(!check_normalized_value("a "));
        assert!(check_normalized_value("a b"));
        assert!(!check_normalized_value("a  b"));
        assert!(!check_normalized_value("a\tb"));
        assert!(check_normalized_value("note-♩"));
    }

    #[test]
    fn independently_constructed_equal_names_are_equal() {
        // The crux of dropping the global interner: equality is by content, so
        // two separately-built names still compare (and hash) equal.
        let a = UniqueString::from("ok");
        let b = UniqueString::from("ok");
        assert_eq!(a, b);
        use std::collections::hash_map::DefaultHasher;
        let mut ha = DefaultHasher::new();
        let mut hb = DefaultHasher::new();
        a.hash(&mut ha);
        b.hash(&mut hb);
        assert_eq!(ha.finish(), hb.finish());
    }

    #[test]
    fn qname_equality_uses_uri_and_local() {
        let ns = NamespaceUri::try_from("ns1").unwrap();
        let a = QName::new_from_parts(NcName::try_from("x").unwrap(), Some(ns.clone()));
        let b = QName::new_from_parts(NcName::try_from("x").unwrap(), Some(ns));
        let c = QName::from_local_name(NcName::try_from("x").unwrap());
        assert_eq!(a, b); // same uri + local
        assert_ne!(a, c); // differing namespace
        assert_eq!(a.local_name().to_string(), "x");
    }

    #[test]
    fn qname_hash_consistent_with_eq() {
        use std::collections::HashMap;
        let mut m: HashMap<QName, u32> = HashMap::new();
        m.insert(QName::try_from("div").unwrap(), 1);
        assert_eq!(m.get(&QName::try_from("div").unwrap()), Some(&1));
        assert_eq!(m.get(&QName::try_from("span").unwrap()), None);
    }
}
