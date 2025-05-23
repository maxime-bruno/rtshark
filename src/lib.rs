//! An interface to [TShark], the famous network protocol analyzer. [TShark] is a part of [Wireshark] distribution.
//! This crate provides an API to start TShark and analyze it's output.
//! It lets you capture packet data from a live network, or read packets from a previously saved capture file, printing a decoded form of those packets.
//! TShark's native capture file format is pcapng format, which is also the format used by Wireshark and various other tools.
//!
//! [Wireshark]: <https://www.wireshark.org/>
//! [TShark]: <https://www.wireshark.org/docs/man-pages/tshark.html>
//!
//! Many information about TShark usage could also be found [here](https://tshark.dev/).
//!
//! TShark application must be installed for this crate to work properly.
//!
//! This crates supports both offline processing (using pcap file) and live analysis (using an interface or a fifo).
//!
//! # Examples
//!
//! ```
//! // Creates a builder with needed tshark parameters
//! let builder = rtshark::RTSharkBuilder::builder()
//!     .input_path("/tmp/my.pcap");
//!
//! // Start a new TShark process
//! let mut rtshark = match builder.spawn() {
//!     Err(err) =>  { eprintln!("Error running tshark: {err}"); return }
//!     Ok(rtshark) => rtshark,
//! };
//!
//! // read packets until the end of the PCAP file
//! while let Some(packet) = rtshark.read().unwrap_or_else(|e| {
//!     eprintln!("Error parsing TShark output: {e}");
//!     None
//! }) {
//!     for layer in packet {
//!         println!("Layer: {}", layer.name());
//!         for metadata in layer {
//!             println!("\t{}", metadata.value());
//!         }
//!     }
//! }
//! ```

use quick_xml::events::{BytesStart, Event};
use semver::Version;
use std::io::{BufRead, BufReader, Error, ErrorKind, Result};
#[cfg(target_family = "unix")]
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};

/// A metadata belongs to one [Layer]. It describes one particular information about a [Packet] (example: IP source address).
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Metadata {
    /// Name displayed by TShark
    name: String,
    /// Value displayed by TShark, in a human readable format
    /// It uses pyshark-like algorithm to display the best 'value' :
    /// it looks for "show" first, then "value", finally "showname"
    value: String,
    /// Value displayed by TShark, if different from human readable format
    raw_value: Option<String>,
    /// Both name and value, as displayed by thshark
    display: Option<String>,
    /// Size of this data extracted from packet header protocol, in bytes
    size: Option<u32>,
    /// Offset of this data in the packet, in bytes
    position: Option<u32>,
}

/// This is one metadata from a given layer of the packet returned by TShark application.
impl Metadata {
    /// Creates a new metadata. This function is useless for most applications.
    pub fn new(
        name: String,
        value: String,
        display: Option<String>,
        size: Option<u32>,
        position: Option<u32>,
    ) -> Metadata {
        Metadata {
            name,
            value,
            raw_value: None,
            display,
            size,
            position,
        }
    }

    /// Get the name of this metadata. The name is returned by TShark.
    ///
    /// # Examples
    ///
    /// ```
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), None, None, None);
    /// assert_eq!(ip_src.name(), "ip.src")
    /// ```
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Value for this metadata, displayed by TShark, in a human readable format.
    /// It uses pyshark-like algorithm to display the best 'value' :
    /// it looks for "show" first, then "value", finally "showname".
    ///
    /// # Examples
    ///
    /// ```
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), None, None, None);
    /// assert_eq!(ip_src.value(), "127.0.0.1")
    /// ```
    pub fn value(&self) -> &str {
        self.value.as_str()
    }

    /// Raw value for this metadata, displayed by TShark.
    ///
    /// This value is not set when using metadata whitelist filtering.
    ///
    /// When `value` is set to "show" instead of "value", "value" can still
    /// be retrieved from `raw_value`.
    pub fn raw_value(&self) -> &str {
        self.raw_value.as_ref().unwrap_or(&self.value).as_str()
    }

    /// Both name and value, as displayed by TShark
    ///
    /// This value is not set when using metadata whitelist filtering.
    ///
    /// # Examples
    ///
    /// ```
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), Some("Source: 127.0.0.1".to_string()), None, None);
    /// assert_eq!(ip_src.display(), Some("Source: 127.0.0.1"))
    /// ```
    pub fn display(&self) -> Option<&str> {
        self.display.as_deref()
    }

    /// Size of this data extracted from packet header protocol, in bytes
    ///
    /// This value is not set when using metadata whitelist filtering.
    ///
    /// # Examples
    ///
    /// ```
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), Some("Source: 127.0.0.1".to_string()), Some(4), Some(12));
    /// assert_eq!(ip_src.size(), Some(4))
    /// ```
    pub fn size(&self) -> Option<u32> {
        self.size
    }

    /// Offset of this data in the packet, in bytes
    ///
    /// This value is not set when using metadata whitelist filtering.
    ///
    /// # Examples
    ///
    /// ```
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), Some("Source: 127.0.0.1".to_string()), Some(4), Some(12));
    /// assert_eq!(ip_src.position(), Some(12))
    /// ```
    pub fn position(&self) -> Option<u32> {
        self.position
    }
}

/// A layer is a protocol in the protocol stack of a packet (example: IP layer). It may contain multiple [Metadata].
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Layer {
    /// Name of this layer
    name: String,
    /// Number of this layer for this packet in the stack of layers. Starts at 0 with "frame" virtual layer.
    index: usize,
    /// List of metadata associated to this layer
    metadata: Vec<Metadata>,
}

impl Layer {
    /// Creates a new layer. This function is useless for most applications.
    ///
    /// # Example
    ///
    /// ```
    /// let ip_layer = rtshark::Layer::new("ip".to_string(), 1);
    /// ```
    pub fn new(name: String, index: usize) -> Self {
        Layer {
            name,
            index,
            metadata: vec![],
        }
    }
    /// Retrieves the layer name of this layer object. This name is a protocol name returned by TShark.
    ///
    /// # Example
    ///
    /// ```
    /// let mut ip_layer = rtshark::Layer::new("ip".to_string(), 1);
    /// assert_eq!(ip_layer.name(), "ip")
    /// ```
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Retrieves this layer index (number of this layer in the stack of the packet's layers).
    ///
    /// # Example
    ///
    /// ```
    /// let mut ip_layer = rtshark::Layer::new("ip".to_string(), 1);
    /// assert_eq!(ip_layer.index(), 1)
    /// ```
    pub fn index(&self) -> usize {
        self.index
    }

    /// Adds a metadata in the list of metadata for this layer. This function is useless for most applications.
    ///
    /// # Example
    ///
    /// ```
    /// let mut ip_layer = rtshark::Layer::new("ip".to_string(), 1);
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), None, None, None);
    /// ip_layer.add(ip_src);
    /// ```
    pub fn add(&mut self, metadata: Metadata) {
        self.metadata.push(metadata);
    }

    /// Get a metadata by its name.
    ///
    /// # Example
    ///
    /// ```
    /// let mut ip_layer = rtshark::Layer::new("ip".to_string(), 1);
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), Some("Source: 127.0.0.1".to_string()), None, None);
    /// ip_layer.add(ip_src);
    /// let ip_src = ip_layer.metadata("ip.src").unwrap();
    /// assert_eq!(ip_src.display(), Some("Source: 127.0.0.1"))
    /// ```
    pub fn metadata(&self, name: &str) -> Option<&Metadata> {
        self.metadata.iter().find(|m| m.name().eq(name))
    }

    /// Get an iterator on the list of [Metadata] for this [Layer].
    /// This iterator does not take ownership of returned [Metadata].
    /// This is the opposite of the "into"-iterator which returns owned objects.
    ///
    /// # Example
    ///
    /// ```
    /// let mut ip_layer = rtshark::Layer::new("ip".to_string(), 1);
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), Some("Source: 127.0.0.1".to_string()), None, None);
    /// ip_layer.add(ip_src);
    /// let metadata = ip_layer.iter().next().unwrap();
    /// assert_eq!(metadata.display(), Some("Source: 127.0.0.1"))
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = &Metadata> {
        self.metadata.iter()
    }
}

impl IntoIterator for Layer {
    type Item = Metadata;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    /// Get an "into" iterator on the list of [Metadata] for this [Layer].
    /// This iterator takes ownership of returned [Metadata].
    /// This is the opposite of an iterator by reference.
    ///
    /// # Example 1
    ///
    /// ```
    /// let mut ip_layer = rtshark::Layer::new("ip".to_string(), 1);
    /// let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), Some("Source: 127.0.0.1".to_string()), None, None);
    /// ip_layer.add(ip_src);
    /// for metadata in ip_layer {
    ///     assert_eq!(metadata.display(), Some("Source: 127.0.0.1"))
    /// }
    /// ```
    /// # Example 2
    ///
    /// ```
    /// # let mut ip_layer = rtshark::Layer::new("ip".to_string(), 1);
    /// # let ip_src = rtshark::Metadata::new("ip.src".to_string(), "127.0.0.1".to_string(), Some("Source: 127.0.0.1".to_string()), None, None);
    /// # ip_layer.add(ip_src);
    /// let metadata = ip_layer.into_iter().next().unwrap();
    /// assert_eq!(metadata.display(), Some("Source: 127.0.0.1"))
    /// ```
    fn into_iter(self) -> Self::IntoIter {
        self.metadata.into_iter()
    }
}

/// The [Packet] object represents a network packet, a formatted unit of data carried by a packet-switched network. It may contain multiple [Layer].
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Packet {
    /// Stack of layers for a packet
    layers: Vec<Layer>,
    /// Packet capture timestamp --- the number of non-leap-microseconds since
    /// January 1, 1970 UTC
    timestamp_micros: Option<i64>,
}

impl Packet {
    /// Creates a new empty layer. This function is useless for most applications.
    /// # Examples
    ///
    /// ```
    /// let packet = rtshark::Packet::new();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns this packet's capture time as the number of non-leap-microseconds since
    /// January 1, 1970 UTC.
    pub fn timestamp_micros(&self) -> Option<i64> {
        self.timestamp_micros
    }

    /// Push a new layer at the end of the layer stack. This function is useless for most applications.
    /// # Examples
    ///
    /// ```
    /// let mut ip_packet = rtshark::Packet::new();
    /// ip_packet.push("ip".to_string());
    /// ```
    pub fn push(&mut self, name: String) {
        let layer = Layer::new(name, self.layers.len());
        self.layers.push(layer);
    }

    /// Push a new layer at the end of the layer stack if the given layer does not exist yet.
    pub fn push_if_not_exist(&mut self, name: String) {
        if let Some(last_layer) = self.last_layer_mut() {
            // ignore the layer if it already exists
            if last_layer.name.eq(&name) {
                return;
            }
        }

        self.push(name);
    }

    /// Get the last layer as mutable reference. It is used to push incoming metadata in the current packet.
    fn last_layer_mut(&mut self) -> Option<&mut Layer> {
        self.layers.last_mut()
    }

    /// Get the layer for the required index. Indexes start at 0.
    /// # Examples
    ///
    /// ```
    /// let mut ip_packet = rtshark::Packet::new();
    /// ip_packet.push("eth".to_string());
    /// ip_packet.push("ip".to_string());
    /// ip_packet.push("tcp".to_string());
    /// assert_eq!(ip_packet.layer_index(0).unwrap().name(), "eth");
    /// assert_eq!(ip_packet.layer_index(1).unwrap().name(), "ip");
    /// assert_eq!(ip_packet.layer_index(2).unwrap().name(), "tcp");
    /// ```
    pub fn layer_index(&self, index: usize) -> Option<&Layer> {
        self.layers.get(index)
    }

    /// Get the layer with the searched name.
    /// If multiple layers have the same name, in case of IP tunnels for instance, the layer with the lowest index is returned.
    /// # Examples
    ///
    /// ```
    /// let mut ip_packet = rtshark::Packet::new();
    /// ip_packet.push("eth".to_string());
    /// ip_packet.push("ip".to_string());
    /// ip_packet.push("ip".to_string());
    /// let ip_layer = ip_packet.layer_name("ip").unwrap();
    /// assert_eq!(ip_layer.index(), 1);
    /// ```
    pub fn layer_name(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|&layer| layer.name.eq(name))
    }

    /// Get the number of layers for this packet.
    /// # Examples
    ///
    /// ```
    /// let mut ip_packet = rtshark::Packet::new();
    /// ip_packet.push("eth".to_string());
    /// ip_packet.push("ip".to_string());
    /// ip_packet.push("tcp".to_string());
    /// assert_eq!(ip_packet.layer_count(), 3);
    /// ```
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Get an iterator on the list of [Layer] for this [Packet].
    /// This iterator does not take ownership of returned data.
    /// This is the opposite of the "into"-iterator which returns owned objects.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut ip_packet = rtshark::Packet::new();
    /// ip_packet.push("ip".to_string());
    /// let layer = ip_packet.iter().next().unwrap();
    /// assert_eq!(layer.name(), "ip")
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = &Layer> {
        self.layers.iter()
    }
}

impl IntoIterator for Packet {
    type Item = Layer;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    /// Get an "into" iterator on the list of [Layer] for this [Packet].
    /// This iterator takes ownership of returned [Layer].
    /// This is the opposite of an iterator by reference.
    ///
    /// # Example 1
    ///
    /// ```
    /// let mut ip_packet = rtshark::Packet::new();
    /// ip_packet.push("ip".to_string());
    /// for layer in ip_packet {
    ///     assert_eq!(layer.name(), "ip")
    /// }
    /// ```
    /// # Example 2
    ///
    /// ```
    /// let mut ip_packet = rtshark::Packet::new();
    /// ip_packet.push("ip".to_string());
    /// let layer = ip_packet.into_iter().next().unwrap();
    /// assert_eq!(layer.name(), "ip")
    /// ```
    fn into_iter(self) -> Self::IntoIter {
        self.layers.into_iter()
    }
}

/// RTSharkBuilder is used to prepare arguments needed to start a TShark instance.
/// When the mandatory input_path is set, it creates a [RTSharkBuilderReady] object,
/// which can be used to add more optional parameters before spawning a [RTShark] instance.
/// RTSharkBuilder may be used to retrieve version information for the TShark executable.
pub struct RTSharkBuilder {}

impl<'a> RTSharkBuilder {
    /// Initial builder function which creates an empty object.
    pub fn builder() -> Self {
        RTSharkBuilder {}
    }

    /// This is the only mandatory parameter, used to provide source of packets.
    /// It enables either -r or -i option of TShark, depending on the use of .live_capture(), see below.
    ///
    /// # Without .live_capture()
    ///
    /// If .live_capture() is not set, TShark will read packet data from a file. It can be any supported capture file format (including gzipped files).
    ///
    /// It is possible to use named pipes or stdin (-) here but only with certain (not compressed) capture file formats
    /// (in particular: those that can be read without seeking backwards).
    ///
    /// ## Example: Prepare an instance of TShark to read a PCAP file
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap");
    /// ```
    ///
    /// # With .live_capture()
    ///
    /// If .live_capture() is set, a network interface or a named pipe can be used to read packets.
    ///
    /// Network interface names should match one of the names listed in "tshark -D" (described above);
    /// a number, as reported by "tshark -D", can also be used.
    ///
    /// If you're using UNIX, "netstat -i", "ifconfig -a" or "ip link" might also work to list interface names,
    /// although not all versions of UNIX support the -a option to ifconfig.
    /// Pipe names should be the name of a FIFO (named pipe).
    ///
    /// On Windows systems, pipe names must be of the form "\\pipe\.*pipename*".
    ///
    /// "TCP@\<host\>:\<port\>" causes TShark to attempt to connect to the specified port on the specified host and read pcapng or pcap data.
    ///
    /// Data read from pipes must be in standard pcapng or pcap format. Pcapng data must have the same endianness as the capturing host.
    ///
    /// ## Example: Prepare an instance of TShark to read from a fifo
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.fifo")
    ///     .live_capture();
    /// ```
    /// ## Example: Prepare an instance of TShark to read from a network interface
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("eth0")
    ///     .live_capture();
    /// ```
    pub fn input_path(&mut self, path: &'a str) -> RTSharkBuilderReady<'a> {
        RTSharkBuilderReady::<'a> {
            input_path: vec![path],
            live_capture: false,
            metadata_blacklist: vec![],
            metadata_whitelist: None,
            capture_filter: "",
            display_filter: "",
            env_path: "",
            options: vec![],
            disabled_protocols: vec![],
            enabled_protocols: vec![],
            output_path: "",
            decode_as: vec![],
        }
    }

    /// Retrieve version information for the TShark executable.
    ///
    /// ## Example:
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder();
    /// if let Ok(version) = builder.version() {
    ///     println!("Version: {}", version.message());
    /// }
    /// ```
    pub fn version(&self) -> Result<RTSharkVersion> {
        let output = Command::new("tshark").args(["--version"]).output()?;
        let message = std::str::from_utf8(&output.stdout)
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Version message not utf8: {}", e),
                )
            })?
            .to_owned();
        let version = message
            .split_whitespace()
            .find_map(|s| Version::parse(s).ok())
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unable to parse version from command output",
            ))?;
        Ok(RTSharkVersion { version, message })
    }
}

/// Version information for the TShark executable
pub struct RTSharkVersion {
    version: Version,
    message: String,
}

impl RTSharkVersion {
    /// The version of the TShark executable.
    ///
    /// This value may be logged or used to check for support for features
    /// not available from all versions of TShark.
    ///
    /// ## Example:
    /// ```
    /// use semver::Version;
    ///
    /// let min_version = Version::new(4, 0, 0);
    /// let builder = rtshark::RTSharkBuilder::builder();
    /// if let Ok(version) = builder.version() {
    ///     if version.version() < &min_version {
    ///         println!("Version requirements not met!");
    ///     }
    /// }
    /// ```
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The full versioning message printed by the TShark executable.
    ///
    /// The full message may include additional information about
    /// copyrights, the environment where the binary was compiled, and
    /// the environment where the binary is currently running.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// RTSharkBuilderReady is an object used to run to create a [RTShark] instance.
/// It is possible to use it to add more optional parameters before starting a TShark application.
#[derive(Clone)]
pub struct RTSharkBuilderReady<'a> {
    /// path to input source
    input_path: Vec<&'a str>,
    /// activate live streaming (fifo, network interface). This activates -i option instead of -r.
    live_capture: bool,
    /// filter out (blacklist) useless metadata names, to prevent storing them in output packet structure
    metadata_blacklist: Vec<String>,
    /// filter out (whitelist) useless metadata names, to prevent TShark to put them in PDML report
    metadata_whitelist: Option<Vec<String>>,
    /// capture_filter : string to be passed to libpcap to filter packets (let pass only packets matching this filter)
    capture_filter: &'a str,
    /// display filter : expression filter to match before TShark prints a packet
    display_filter: &'a str,
    /// custom environment path containing TShark application
    env_path: &'a str,
    /// any special options to configure protocol decoding
    options: Vec<String>,
    /// any protocols that should be explicitly disabled
    disabled_protocols: Vec<String>,
    /// any protocols that should be explicitly enabled
    enabled_protocols: Vec<String>,
    /// path to input source
    output_path: &'a str,
    /// decode_as : let TShark to decode as this expression
    decode_as: Vec<&'a str>,
}

impl<'a> RTSharkBuilderReady<'a> {
    /// Adds another input for tshark. It works only with live capture to read packets from
    /// multiple interfaces.
    /// Adding multiple pcap files will fail, since tshark will only read the last instance of "-r"
    /// option.
    ///
    /// ## Example: Prepare an instance of TShark to read from multiple network interfaces
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("eth0")
    ///     .input_path("eth1")
    ///     .live_capture();
    /// ```
    #[must_use]
    pub fn input_path(&self, path: &'a str) -> Self {
        let mut new = self.clone();
        new.input_path.push(path);
        new
    }

    /// Enables -i option of TShark.
    ///
    /// This option must be set to use network interface or pipe for live packet capture. See input_path() option of [RTSharkBuilder] for more details.
    ///
    #[must_use]
    pub fn live_capture(&self) -> Self {
        let mut new = self.clone();
        new.live_capture = true;
        new
    }

    /// Filter expression to be passed to libpcap to filter captured packets.
    ///
    /// Warning: these capture filters cannot be specified when reading a capture file.
    /// There are enabled only when using live_capture(). This filter will be ignored if live_capture() is not set.
    ///
    /// Packet capturing filter is performed with the pcap library.
    /// That library supports specifying a filter expression; packets that don't match that filter are discarded.
    /// The syntax of a capture filter is defined by the pcap library.
    /// This syntax is different from the TShark filter syntax.
    ///
    /// More information about libpcap filters here : <https://www.tcpdump.org/manpages/pcap-filter.7.html>
    ///
    /// ### Example: Prepare an instance of TShark with packet capture filter.
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("eth0")
    ///     .live_capture()
    ///     .capture_filter("port 53");
    /// ```
    #[must_use]
    pub fn capture_filter(&self, filter: &'a str) -> Self {
        let mut new = self.clone();
        new.capture_filter = filter;
        new
    }

    /// Expression applied on analyzed packet metadata to print and write only matching packets.
    ///
    /// Cause the specified filter (which uses the syntax of read/display filters, rather than that of capture filters)
    /// to be applied before printing a decoded form of packets or writing packets to a file.
    /// Packets matching the filter are printed or written to file; packets that the matching packets depend upon (e.g., fragments),
    /// are not printed but are written to file; packets not matching the filter nor depended upon are discarded rather than being printed or written.
    ///
    /// ### Example: Prepare an instance of TShark with display filter.
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap")
    ///     .display_filter("udp.port == 53");
    /// ```
    #[must_use]
    pub fn display_filter(&self, filter: &'a str) -> Self {
        let mut new = self.clone();
        new.display_filter = filter;
        new
    }

    /// Filter out (blacklist) a list of useless metadata names extracted by TShark,
    /// to prevent storing them in [Packet] structure and consume extra memory.
    /// Filtered [Metadata] will not be available in [Packet]'s [Layer].
    ///
    /// This method can be called multiple times to add more metadata in the blacklist.
    ///
    /// ### Example: Prepare an instance of TShark with IP source and destination metadata filtered.
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap")
    ///     .metadata_blacklist("ip.src")
    ///     .metadata_blacklist("ip.dst");
    /// ```
    #[must_use]
    pub fn metadata_blacklist(&self, blacklist: &'a str) -> Self {
        let mut new = self.clone();
        new.metadata_blacklist.push(blacklist.to_owned());
        new
    }

    /// Filter out (whitelist) a list of needed metadata names to be extracted by TShark,
    /// to prevent it to extract and put everything in the PDML report.
    /// There is a huge performance gain for TShark if the whitelist is small.
    /// Filtered [Metadata] will not be available in [Packet]'s [Layer].
    ///
    /// This method can be called multiple times to add more metadata in the whitelist.
    ///
    /// In whitelist mode, TShark PDML does not encapsulate fields in a 'proto' tag anymore
    /// so it is not possible to build all packet's layers.
    ///
    /// ### Example: Prepare an instance of TShark to print only IP source and destination metadata.
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap")
    ///     .metadata_whitelist("ip.src")
    ///     .metadata_whitelist("ip.dst");
    /// ```
    #[must_use]
    pub fn metadata_whitelist(&self, whitelist: &'a str) -> Self {
        let mut new = self.clone();
        if let Some(wl) = &mut new.metadata_whitelist {
            wl.push(whitelist.to_owned());
        } else {
            new.metadata_whitelist = Some(vec![whitelist.to_owned()]);
        }
        new
    }

    /// Replace the PATH environment variable. This is used to specify where to look for tshark executable.
    ///
    /// Note that environment variable names are case-insensitive (but case-preserving) on Windows,
    /// and case-sensitive on all other platforms.
    ///
    /// ### Example: Prepare an instance of TShark when binary is installed in a custom path
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap")
    ///     .env_path("/opt/local/tshark/");
    /// ```
    #[must_use]
    pub fn env_path(&self, path: &'a str) -> Self {
        let mut new = self.clone();
        new.env_path = path;
        new
    }

    /// Specify the key log file that enables decryption of TLS traffic.
    ///
    /// The key log file is generated by the browser when `SSLKEYLOGFILE` environment variable
    /// is set. See <https://wiki.wireshark.org/TLS#using-the-pre-master-secret> for more
    /// details.
    ///
    /// Note that you can embed the TLS key log file in a capture file:
    ///
    /// ```no_compile
    /// editcap --inject-secrets tls,keys.txt in.pcap out-dsb.pcapng
    /// ```
    #[must_use]
    pub fn keylog_file(&self, path: &'a str) -> Self {
        let mut new = self.clone();
        new.options.push(format!("tls.keylog_file:{}", path));
        new
    }

    /// Set custom protocol's option to tune the tshark decoding.
    /// This adds -o parameter to tshark command line.
    ///
    /// This method can be called multiple times to add more options.
    ///
    /// ### Example: Prepare an instance of TShark without ip defragmenting and custom inap args:
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap")
    ///     .option("ip.defragment:false")
    ///     .option("inap.ssn:146");
    /// ```
    #[must_use]
    pub fn option(&self, option: &'a str) -> Self {
        let mut new = self.clone();
        new.options.push(option.to_owned());
        new
    }

    /// Provide protocol names that should be disabled in tshark decoding.
    ///
    /// This method can be called multiple times to add more protocols.
    ///
    /// ### Example: Prepare an instance of TShark where t30 and t38 protocols are not decoded:
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap")
    ///     .disable_protocol("t30")
    ///     .disable_protocol("t38");
    /// ```
    #[must_use]
    pub fn disable_protocol(&self, protocol: &'a str) -> Self {
        let mut new = self.clone();
        new.disabled_protocols.push(protocol.to_owned());
        new
    }

    /// Provide protocol names that should be enabled in tshark decoding.
    ///
    /// This method can be called multiple times to add more protocols.
    ///
    /// ### Example: Prepare an instance of TShark where only ethernet and ip are decoded:
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap")
    ///     .disable_protocol("ALL")
    ///     .enable_protocol("eth")
    ///     .enable_protocol("ip");
    /// ```
    #[must_use]
    pub fn enable_protocol(&self, protocol: &'a str) -> Self {
        let mut new = self.clone();
        new.enabled_protocols.push(protocol.to_owned());
        new
    }

    /// Write raw packet data to outfile or to the standard output if outfile is '-'.
    /// Note : this option provides raw packet data, not text.
    ///
    /// ### Example: Prepare an instance of TShark to store raw packet data
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/in.pcap")
    ///     .output_path("/tmp/out.pcap");
    /// ```
    #[must_use]
    pub fn output_path(&self, path: &'a str) -> Self {
        let mut new = self.clone();
        new.output_path = path;
        new
    }

    /// Let TShark to decode as the protocol which specified in the expression.
    ///
    /// This method can be called multiple times to add more expression in the decode_as list.
    ///
    /// ### Example: The packet which has TCP port 8080 or 8081 is decoded as HTTP/2.
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap")
    ///     .decode_as("tcp.port==8080,http2")
    ///     .decode_as("tcp.port==8081,http2");
    /// ```
    #[must_use]
    pub fn decode_as(&self, expr: &'a str) -> Self {
        let mut new = self.clone();
        new.decode_as.push(expr);
        new
    }

    /// Starts a new TShark process given the provided parameters, mapped to a new [RTShark] instance.
    /// This function may fail if tshark binary is not in PATH or if there are some issues with input_path parameter : not found or no read permission...
    /// In other cases (output_path not writable, invalid syntax for pcap_filter or display_filter),
    /// TShark process will start but will stop a few moments later, leading to a EOF on rtshark.read function.
    /// # Example
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap");
    /// let tshark: std::io::Result<rtshark::RTShark> = builder.spawn();
    /// ```
    pub fn spawn(&self) -> Result<RTShark> {
        let mut tshark_params = self.prepare_args()?;

        tshark_params.extend(&[
            // Packet Details Markup Language, an XML-based format for the details of a decoded packet.
            // This information is equivalent to the packet details printed with the -V option.
            "-Tpdml", // -l activate unbuffered mode, useful to print packets as they come
            "-l",
        ]);

        let mut tshark_child = self.spawn_tshark(&tshark_params)?;

        let buf_reader = BufReader::new(tshark_child.stdout.take().unwrap());
        let stderr = BufReader::new(tshark_child.stderr.take().unwrap());

        let reader = quick_xml::Reader::from_reader(buf_reader);

        Ok(RTShark::new(
            tshark_child,
            reader,
            stderr,
            self.metadata_blacklist.clone(),
        ))
    }

    /// Starts a new TShark process given the provided parameters and runs it to completion. In
    /// contrast to [`RTSharkBuilderReady::spawn` ]no programmatic access to individual packets is
    /// provided.
    /// This function may fail if tshark binary is not in PATH or if there are some issues with input_path parameter : not found or no read permission...
    /// In other cases (output_path not writable, invalid syntax for pcap_filter or display_filter),
    /// TShark process will fail and the stderr will be reported.
    /// # Example
    ///
    /// ```
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap");
    /// let _: Result<(), std::io::Error> = builder.batch();
    /// ```
    pub fn batch(&self) -> Result<()> {
        let tshark_params = self.prepare_args()?;

        let mut tshark_child = self.spawn_tshark(&tshark_params)?;

        if !tshark_child.wait()?.success() {
            let mut stderr = BufReader::new(tshark_child.stderr.take().unwrap());
            // if process stops, there may be due to an error, we can get it in stderr
            let mut line = String::new();
            let size = stderr.read_line(&mut line)?;
            // if len is != 0 there is an error message
            if size != 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, line));
            }
        }

        Ok(())
    }

    fn spawn_tshark(&self, tshark_params: &[&str]) -> Result<Child> {
        // piping from TShark, not to load the entire output in ram...
        // spawn may fail if TShark is not found in path

        let tshark_child = if self.env_path.is_empty() {
            Command::new("tshark")
                .args(tshark_params)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        } else {
            Command::new("tshark")
                .args(tshark_params)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env("PATH", self.env_path)
                .spawn()
        };

        tshark_child.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                std::io::Error::new(e.kind(), format!("Unable to find tshark: {}", e))
            }
            _ => e,
        })
    }

    /// Prepare tshark command line parameters.
    fn prepare_args(&self) -> Result<Vec<&str>> {
        let mut tshark_params = if self.live_capture {
            let mut input = vec![];
            self.input_path
                .iter()
                .for_each(|i| input.extend(&["-i", i]));
            input
        } else {
            if self.input_path.len() > 1 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "tshark supports only one input pcap file",
                ));
            }

            // test if input file exists
            let input_path = self.input_path[0];
            std::fs::metadata(input_path).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    std::io::Error::new(e.kind(), format!("Unable to find {}: {}", input_path, e))
                }
                _ => e,
            })?;

            vec!["-r", input_path]
        };

        tshark_params.extend(&[
            // Disable network object name resolution (such as hostname, TCP and UDP port names)
            "-n",
            // When capturing packets, TShark writes to the standard error an initial line listing the interfaces from which packets are being captured and,
            // if packet information isn’t being displayed to the terminal, writes a continuous count of packets captured to the standard output.
            // If the -Q option is specified, neither the initial line, nor the packet information, nor any packet counts will be displayed.
            "-Q",
        ]);

        if !self.output_path.is_empty() {
            tshark_params.extend(&["-w", self.output_path]);
        }

        if self.live_capture && !self.capture_filter.is_empty() {
            tshark_params.extend(&["-f", self.capture_filter]);
        }

        if !self.display_filter.is_empty() {
            tshark_params.extend(&["-Y", self.display_filter]);
        }

        if !self.decode_as.is_empty() {
            for elm in self.decode_as.iter() {
                tshark_params.extend(&["-d", elm]);
            }
        }

        for option in &self.options {
            tshark_params.extend(&["-o", option]);
        }

        if let Some(wl) = &self.metadata_whitelist {
            for whitelist_elem in wl {
                tshark_params.extend(&["-e", whitelist_elem]);
            }
        }

        for protocol in &self.disabled_protocols {
            tshark_params.extend(&["--disable-protocol", protocol]);
        }

        for protocol in &self.enabled_protocols {
            tshark_params.extend(&["--enable-protocol", protocol]);
        }

        Ok(tshark_params)
    }
}

/// RTShark structure represents a TShark process.
/// It allows controlling the TShark process and reading from application's output.
/// It is created by [RTSharkBuilder].
pub struct RTShark {
    /// Contains the TShark process handle, when TShark is running
    process: Option<Child>,
    /// xml parser on TShark piped output
    parser: quick_xml::Reader<BufReader<ChildStdout>>,
    /// stderr
    stderr: BufReader<ChildStderr>,
    /// optional metadata blacklist, to prevent storing useless metadata in output packet structure
    filters: Vec<String>,
}

impl RTShark {
    /// create a new RTShark instance from a successful builder call.
    fn new(
        process: Child,
        parser: quick_xml::Reader<BufReader<ChildStdout>>,
        stderr: BufReader<ChildStderr>,
        filters: Vec<String>,
    ) -> Self {
        RTShark {
            process: Some(process),
            parser,
            stderr,
            filters,
        }
    }

    /// Read a packet from thsark output and map it to the [Packet] type.
    /// Reading packet can be done until 'None' is returned.
    /// Once 'None' is returned, no more packets can be read from this stream
    /// and TShark instance can be dropped.
    /// This could happen when TShark application dies or when this is the end of the PCAP file.
    ///
    /// # Example
    ///
    /// ```
    /// # // Creates a builder with needed TShark parameters
    /// # let builder = rtshark::RTSharkBuilder::builder()
    /// #     .input_path("/tmp/my.pcap");
    /// // Start a new TShark process
    /// let mut rtshark = match builder.spawn() {
    ///     Err(err) => { eprintln!("Error running tshark: {err}"); return; }
    ///     Ok(rtshark) => rtshark
    /// };
    ///
    /// // read packets until the end of the PCAP file
    /// loop {
    ///     let packet = match rtshark.read() {
    ///         Ok(p) => p,
    ///         Err(e) => { eprintln!("Got decoding error: {e}"); continue; }
    ///     };
    ///
    ///     // end of stream
    ///     if let None = packet {
    ///         break;
    ///     }
    ///
    ///     println!("Got a packet");
    /// }
    /// ```
    pub fn read(&mut self) -> Result<Option<Packet>> {
        let xml_reader = &mut self.parser;

        let msg = parse_xml(xml_reader, &self.filters);
        if let Ok(ref msg) = msg {
            let done = match msg {
                None => {
                    // Got None == EOF
                    match self.process {
                        Some(ref mut process) => RTShark::try_wait_has_exited(process),
                        _ => true,
                    }
                }
                _ => false,
            };

            if done {
                self.process = None;

                // if process stops, there may be due to an error, we can get it in stderr
                let mut line = String::new();
                let size = self.stderr.read_line(&mut line)?;
                // if len is != 0 there is an error message
                if size != 0 {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, line));
                }
            }
        }

        msg
    }

    /// Kill the running TShark process associated to this rtshark instance.
    /// Once TShark is killed, there is no way to start it again using this object.
    /// Any new TShark instance has to be created using RTSharkBuilder.
    ///
    /// # Example
    ///
    /// ```
    /// // Creates a builder with needed TShark parameters
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap");
    ///
    /// // Start a new TShark process
    /// let mut rtshark = match builder.spawn() {
    ///     Err(err) => { eprintln!("Error running tshark: {err}"); return; }
    ///     Ok(rtshark) => rtshark
    /// };
    ///
    /// // kill running TShark process
    /// rtshark.kill();
    /// ```
    pub fn kill(&mut self) {
        if let Some(ref mut process) = self.process {
            let done = match process.try_wait() {
                Ok(maybe) => match maybe {
                    None => false,
                    Some(_exitcode) => true,
                },
                Err(e) => {
                    eprintln!("Error while killing rtshark: wait: {e}");
                    false
                }
            };

            if !done {
                match process.kill() {
                    Ok(()) => (),
                    Err(e) => eprintln!("Error while killing rtshark: kill: {e}"),
                }
                if let Err(e) = process.wait() {
                    eprintln!("Error while killing rtshark: wait: {e}");
                }
            }

            self.process = None;
        }
    }

    /// Returns tshark process id if tshark is running.
    /// # Example
    ///
    /// ```
    /// // Creates a builder with needed tshark parameters
    /// let builder = rtshark::RTSharkBuilder::builder()
    ///     .input_path("/tmp/my.pcap");
    ///
    /// // Start a new tshark process
    /// let mut rtshark = match builder.spawn() {
    ///     Err(err) => { eprintln!("Error running tshark: {err}"); return; }
    ///     Ok(rtshark) => println!("tshark PID is {}", rtshark.pid().unwrap())
    /// };
    ///
    /// ```
    pub fn pid(&self) -> Option<u32> {
        self.process.as_ref().map(|p| p.id())
    }

    /// Check if process is stopped, get the exit code and return true if stopped.
    /// Why not doing a simple wait ?
    fn try_wait_has_exited(child: &mut Child) -> bool {
        let mut count = 3;
        while count != 0 {
            #[cfg(target_family = "unix")]
            if let Ok(Some(s)) = child.try_wait() {
                return s.code().is_some() || s.signal().is_some();
            }

            #[cfg(target_family = "windows")]
            if let Ok(Some(s)) = child.try_wait() {
                return s.code().is_some();
            }

            std::thread::sleep(std::time::Duration::from_millis(100));
            count -= 1;
        }

        false
    }
}

impl Drop for RTShark {
    fn drop(&mut self) {
        self.kill()
    }
}

/// search for an attribute of a XML tag using its name and return a string.
fn rtshark_attr_by_name(tag: &BytesStart, key: &[u8]) -> Result<String> {
    let attrs = &mut tag.attributes();
    for attr in attrs {
        let attr = attr.map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Error decoding xml attribute: {e:?}"),
            )
        })?;
        if attr.key.as_ref() == key {
            let value = std::str::from_utf8(&attr.value).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Error decoding utf8 value: {e:?}"),
                )
            })?;
            return Ok(value.to_owned());
        }
    }

    let line =
        std::str::from_utf8(tag.attributes_raw()).unwrap_or("Unable to decode UTF8 XML buffer");

    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "xml lookup error: no key '{}' in '{}'",
            std::str::from_utf8(key).unwrap(),
            line
        ),
    ))
}

/// search for an attribute of a XML tag using its name and return a u32.
fn rtshark_attr_by_name_u32(tag: &BytesStart, key: &[u8]) -> Result<u32> {
    match rtshark_attr_by_name(tag, key) {
        Err(e) => Err(e),
        Ok(v) => v.parse::<u32>().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Error decoding u32 value: {e:?}"),
            )
        }),
    }
}

/// Build a metadata using attributes available on this XML "field" tag.
/// Sample XML line : <field name="frame.time" show="test time" pos="0" size="0" showname="test time display"/>
fn rtshark_build_metadata(tag: &BytesStart, filters: &[String]) -> Result<Option<Metadata>> {
    let name = rtshark_attr_by_name(tag, b"name")?;

    // skip "_ws.expert" info, not related to a packet metadata
    if name.is_empty() || name.starts_with("_ws.") {
        return Ok(None);
    }

    // skip data
    if filters.contains(&name) {
        return Ok(None);
    }

    // Issue #1 : uses pyshark-like algorithm to display the best 'value' for this field
    // https://github.com/KimiNewt/pyshark/blob/master/src/pyshark/packet/fields.py#L14
    // try first "show", then "value", finally "showname"
    let value = match rtshark_attr_by_name(tag, b"show") {
        Ok(value) => Ok(value),
        Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => {
            match rtshark_attr_by_name(tag, b"value") {
                Ok(value) => Ok(value),
                Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => {
                    if let Ok(value) = rtshark_attr_by_name(tag, b"showname") {
                        Ok(value)
                    } else {
                        Err(err)
                    }
                }
                Err(err) => Err(err),
            }
        }
        Err(err) => Err(err),
    }?;

    let mut metadata = Metadata::new(name, value, None, None, None);

    if let Ok(position) = rtshark_attr_by_name_u32(tag, b"pos") {
        metadata.position = Some(position);
    }
    if let Ok(size) = rtshark_attr_by_name_u32(tag, b"size") {
        metadata.size = Some(size);
    }
    if let Ok(display) = rtshark_attr_by_name(tag, b"showname") {
        metadata.display = Some(display);
    }
    if let Ok(raw_value) = rtshark_attr_by_name(tag, b"value") {
        if raw_value != metadata.value {
            metadata.raw_value = Some(raw_value);
        }
    }
    Ok(Some(metadata))
}

/// Process specific metadata in geninfo to fill the packet structure
fn geninfo_metadata(tag: &BytesStart, packet: &mut Packet) -> Result<()> {
    use chrono::{LocalResult, TimeZone as _, Utc};

    let name = rtshark_attr_by_name(tag, b"name")?;
    if name != "timestamp" {
        return Ok(());
    }
    let value = rtshark_attr_by_name(tag, b"value")?;

    let bad_timestamp = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Error decoding timestamp: {value}"),
        )
    };

    let (secs, nsecs) = value.split_once('.').ok_or_else(bad_timestamp)?;
    let secs = secs.parse().map_err(|_| bad_timestamp())?;
    let nsecs = nsecs.parse().map_err(|_| bad_timestamp())?;

    let LocalResult::Single(dt) = Utc.timestamp_opt(secs, nsecs) else {
        return Err(bad_timestamp());
    };
    packet.timestamp_micros.replace(dt.timestamp_micros());

    Ok(())
}

/// list of protocols in tshark output but not in packet data
fn ignored_protocols(name: &str) -> bool {
    name.eq("geninfo") || name.eq("fake-field-wrapper")
}

/// Main parser function used to decode XML output from tshark
fn parse_xml<B: BufRead>(
    xml_reader: &mut quick_xml::Reader<B>,
    filters: &[String],
) -> Result<Option<Packet>> {
    let mut buf = vec![];
    let mut packet = Packet::new();

    let mut protoname = None;

    // tshark pdml is something like : (default mode)
    //
    // <!-- You can find pdml2html.xsl in /usr/share/wireshark or at https://gitlab.com/wireshark/wireshark/-/raw/master/pdml2html.xsl. -->
    // <pdml version="0" creator="wireshark/4.0.6" time="Sat Oct  7 09:51:54 2023" capture_file="src/test.pcap">
    // <packet>
    //   <proto name="geninfo" pos="0" showname="General information" size="28">
    //     <field name="num" pos="0" show="1" showname="Number" value="1" size="28"/>
    //   </proto>
    //   <proto name="frame" pos="0" showname="General information" size="28">
    //   ...
    //
    // or, if using "whitelist" with -e option
    //
    // <pdml version="0" creator="wireshark/4.0.6" time="Sat Oct  7 09:51:54 2023" capture_file="src/test.pcap">
    // <packet>
    //   <proto name="geninfo" pos="0" showname="General information" size="28">
    //     <field name="num" pos="0" show="1" showname="Number" value="1" size="28"/>
    // </proto>
    // <field name="num" pos="0" show="1" showname="Number" value="1" size="28"/>
    // ...

    /// Create a new layer if required and add metadata to the given packet.
    fn _add_metadata(packet: &mut Packet, metadata: Metadata) -> Result<()> {
        // Create a new layer if the field's protocol does not exist yet as a layer.
        if let Some(proto) = metadata.name().split('.').next() {
            packet.push_if_not_exist(proto.to_owned());
        }

        if let Some(layer) = packet.last_layer_mut() {
            layer.add(metadata);
        } else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Cannot find protocol name to push a metadata",
            ));
        }

        Ok(())
    }

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                // Here we have "packet" and "proto" and sometimes "field" tokens. Only "proto" and "field" are interesting today.
                if b"proto" == e.name().as_ref() {
                    let proto = rtshark_attr_by_name(e, b"name")?;
                    protoname = Some(proto.to_owned());

                    // If we face a new protocol, add it in the packet layers stack.
                    if !ignored_protocols(proto.as_str()) {
                        packet.push(proto);
                    }
                }

                // There are cases where fields are mapped in fields. So check if there is any parent field and extract its metadata.
                if b"field" == e.name().as_ref() {
                    if let Some(metadata) = rtshark_build_metadata(e, filters)? {
                        _add_metadata(&mut packet, metadata)?;
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                // Here we should not have anything else than "field" but do a test anyway.
                if b"field" == e.name().as_ref() {
                    // Here we have two cases : with or without encapsuling "proto"
                    // We have a protocol if "whitelist" mode is disabled.
                    // Protocol "geninfo" is always here.
                    if let Some(name) = protoname.as_ref() {
                        if name == "geninfo" {
                            // Put geninfo metadata in packet's object (timestamp ...).
                            geninfo_metadata(e, &mut packet)?;
                        } else if let Some(metadata) = rtshark_build_metadata(e, filters)? {
                            // Some dissectors place field items at the top level instead
                            // of inside a protocol. In these cases, in the PDML output the
                            // field items are placed inside a fake "<proto>" element named
                            // "fake-field-wrapper" in order to maximize compliance.
                            // See https://github.com/wireshark/wireshark/blob/master/doc/README.xml-output
                            //
                            // An example is "tcp.reassembled". We should try to add these
                            // items to the correct layer so that they are accessible.
                            if name == "fake-field-wrapper" {
                                let proto_from_name =
                                    metadata.name().split('.').next().unwrap_or("");
                                let proto_layer = packet
                                    .last_layer_mut()
                                    .filter(|layer| layer.name == proto_from_name);
                                if let Some(layer) = proto_layer {
                                    layer.add(metadata);
                                }
                            } else {
                                // We can unwrap because we must have a layer : it was pushed in Event::Start
                                packet.last_layer_mut().unwrap().add(metadata);
                            }
                        }
                    } else if let Some(metadata) = rtshark_build_metadata(e, filters)? {
                        _add_metadata(&mut packet, metadata)?;
                    }
                }
            }
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"packet" => return Ok(Some(packet)),
                b"proto" => protoname = None,
                _ => (),
            },

            Ok(Event::Eof) => {
                return Ok(None);
            }
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "xml parsing error: {} at tshark output offset {}",
                        e,
                        xml_reader.buffer_position()
                    ),
                ));
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serial_test::serial;

    use super::*;

    #[test]
    fn test_parse_single_proto_metadata() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
           <field name="frame.time" show="test time" pos="0" size="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 1);
        for layer in pkt.layers {
            for m in layer {
                assert!(m.name().eq("frame.time"));
                assert!(m.value().eq("test time"));
                assert_eq!(m.display(), Some("test time display"));
            }
        }
    }

    #[test]
    fn test_parse_missing_optional_size() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field name="frame.time" show="test time" pos="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 1);
    }

    #[test]
    fn test_parse_missing_optional_pos() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field name="frame.time" show="test time" size="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 1);
    }

    #[test]
    fn test_parse_missing_optional_display() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field name="frame.time" show="test time" pos="0" size="0" />
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 1);
    }

    #[test]
    fn test_parse_missing_mandatory_name() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field show="test time" pos="0" size="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = parse_xml(&mut reader, &[]);

        match msg {
            Err(_) => (),
            _ => panic!("invalid result"),
        }
    }

    #[test]
    fn test_parse_all_value_fields_available() {
        // Issue #1 : uses pyshark-like algorithm to display the best 'value' for this field
        // https://github.com/KimiNewt/pyshark/blob/master/src/pyshark/packet/fields.py#L14
        // try first "show", then "value", finally "showname"

        let xml = r#"
        <pdml>
         <packet>
          <proto name="icmp">
           <field name="data" show="data is aa" value="0a" showname="data: a0"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let pkt = parse_xml(&mut reader, &[]).unwrap().unwrap();

        let icmp = pkt.layer_name("icmp").unwrap();
        let data = icmp.metadata("data").unwrap();
        assert!(data.value().eq("data is aa"));
        assert!(data.raw_value().eq("0a"));
        assert_eq!(data.display(), Some("data: a0"));
    }

    #[test]
    fn test_parse_missing_show_attribute() {
        // Issue #1 : uses pyshark-like algorithm to display the best 'value' for this field
        // https://github.com/KimiNewt/pyshark/blob/master/src/pyshark/packet/fields.py#L14
        // try first "show", then "value", finally "showname"

        let xml = r#"
        <pdml>
         <packet>
          <proto name="icmp">
           <field name="data" value="0a" showname="data: a0"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let pkt = parse_xml(&mut reader, &[]).unwrap().unwrap();

        let icmp = pkt.layer_name("icmp").unwrap();
        let data = icmp.metadata("data").unwrap();
        assert!(data.value().eq("0a"));
        assert!(data.raw_value() == data.value());
    }

    #[test]
    fn test_parse_missing_show_and_value_attributes() {
        // Issue #1 : uses pyshark-like algorithm to display the best 'value' for this field
        // https://github.com/KimiNewt/pyshark/blob/master/src/pyshark/packet/fields.py#L14
        // try first "show", then "value", finally "showname"

        let xml = r#"
        <pdml>
         <packet>
          <proto name="icmp">
           <field name="data" showname="data: a0"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let pkt = parse_xml(&mut reader, &[]).unwrap().unwrap();

        let icmp = pkt.layer_name("icmp").unwrap();
        let data = icmp.metadata("data").unwrap();
        assert!(data.value().eq("data: a0"));
        assert!(data.raw_value() == data.value());
    }

    #[test]
    fn test_parse_missing_any_show() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field name="frame.time" pos="0" size="0"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = parse_xml(&mut reader, &[]);
        match msg {
            Err(_) => (),
            _ => panic!("invalid result"),
        }
    }

    const XML_TCP: &str = r#"
    <pdml>
     <packet>
      <proto name="frame">
       <field name="frame.time" show="Mar  5, 2021 08:49:52.736275000 CET"/>
      </proto>
      <proto name="ip">
       <field name="ip.src" show="1.1.1.1" />
       <field name="ip.dst" show="1.1.1.2" />
      </proto>
      <proto name="tcp">
       <field name="tcp.srcport" show="52796" value="ce3c"/>
       <field name="tcp.dstport" show="5432" value="1538"/>
       <field name="tcp.seq_raw" show="1963007432" value="75011dc8"/>
       <field name="tcp.stream" show="4"/>
      </proto>
     </packet>
    </pdml>"#;

    #[test]
    fn test_access_packet_into_iter() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 3);

        let mut iter = pkt.into_iter();
        let frame = iter.next().unwrap();
        assert!(frame.name().eq("frame"));
        let ip = iter.next().unwrap();
        assert!(ip.name().eq("ip"));
        let tcp = iter.next().unwrap();
        assert!(tcp.name().eq("tcp"));
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_access_packet_iter() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 3);

        let mut iter = pkt.iter();
        let frame = iter.next().unwrap();
        assert!(frame.name().eq("frame"));
        let ip = iter.next().unwrap();
        assert!(ip.name().eq("ip"));
        let tcp = iter.next().unwrap();
        assert!(tcp.name().eq("tcp"));
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_access_layer_index() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 3);

        assert!(pkt.layer_index(0).unwrap().name().eq("frame"));
        assert!(pkt.layer_index(1).unwrap().name().eq("ip"));
        assert!(pkt.layer_index(2).unwrap().name().eq("tcp"));

        assert!(pkt.layer_index(3).is_none());
    }

    #[test]
    fn test_access_layer_name() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 3);

        assert!(pkt.layer_name("frame").unwrap().name().eq("frame"));
        assert!(pkt.layer_name("ip").unwrap().name().eq("ip"));
        assert!(pkt.layer_name("tcp").unwrap().name().eq("tcp"));

        assert!(pkt.layer_name("udp").is_none());
    }

    #[test]
    fn test_access_layer_name_with_tunnel() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
           <field name="frame.time" show="Mar  5, 2021 08:49:52.736275000 CET"/>
          </proto>
          <proto name="ip">
           <field name="ip.src" show="10.215.215.9" />
           <field name="ip.dst" show="10.215.215.10" />
          </proto>
          <proto name="ip">
           <field name="ip.src" show="10.10.215.9" />
           <field name="ip.dst" show="10.10.215.10" />
          </proto>
          <proto name="tcp">
           <field name="tcp.srcport" show="52796" value="ce3c"/>
           <field name="tcp.dstport" show="5432" value="1538"/>
           <field name="tcp.seq_raw" show="1963007432" value="75011dc8"/>
           <field name="tcp.stream" show="4"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 4);

        assert!(pkt.layer_name("frame").unwrap().name().eq("frame"));
        assert!(pkt.layer_name("ip").unwrap().name().eq("ip"));
        assert!(pkt.layer_name("ip").unwrap().index() == 1usize);
        assert!(pkt.layer_index(1).unwrap().name().eq("ip"));
        assert!(pkt.layer_index(2).unwrap().name().eq("ip"));
        assert!(pkt.layer_name("tcp").unwrap().name().eq("tcp"));

        assert!(pkt.layer_name("udp").is_none());
    }

    #[test]
    fn test_access_layer_iter() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        let mut iter = ip.iter();
        assert!(iter.next().unwrap().name().eq("ip.src"));
        assert!(iter.next().unwrap().name().eq("ip.dst"));
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_access_layer_into_iter() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap().clone();
        let mut iter = ip.into_iter();
        assert!(iter.next().unwrap().name().eq("ip.src"));
        assert!(iter.next().unwrap().name().eq("ip.dst"));
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_access_layer_metadata() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = parse_xml(&mut reader, &[]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        let src = ip.metadata("ip.src").unwrap();
        assert!(src.value().eq("1.1.1.1"));

        let dst = ip.metadata("ip.dst").unwrap();
        assert!(dst.value().eq("1.1.1.2"));
    }

    #[test]
    fn test_parser_filter_metadata() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = parse_xml(&mut reader, &["ip.src".to_string()]).unwrap();
        let pkt = match msg {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        assert!(ip.metadata("ip.src").is_none());
        assert!(ip.metadata("ip.dst").unwrap().value().eq("1.1.1.2"));
    }

    #[test]
    fn test_parser_multiple_packets() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="tcp"></proto>
         </packet>
         <packet>
          <proto name="udp"></proto>
         </packet>
         <packet>
          <proto name="igmp"></proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));
        match parse_xml(&mut reader, &[]).unwrap() {
            Some(p) => assert!(p.layer_name("tcp").is_some()),
            _ => panic!("invalid Output type"),
        }
        match parse_xml(&mut reader, &[]).unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }
        match parse_xml(&mut reader, &[]).unwrap() {
            Some(p) => assert!(p.layer_name("igmp").is_some()),
            _ => panic!("invalid Output type"),
        }
        match parse_xml(&mut reader, &[]).unwrap() {
            None => (),
            _ => panic!("invalid Output type"),
        }
    }

    #[test]
    fn test_rtshark_field_in_field() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="btcommon">
            <field name="btcommon.eir_ad.entry.data" showname="Data: <data>" size="8" pos="39" show="<some data>" value="<some data>">
              <field name="_ws.expert" showname="Expert Info (Note/Undecoded): Undecoded" size="0" pos="39">
                <field name="btcommon.eir_ad.undecoded" showname="Undecoded" size="0" pos="0" show="" value=""/>
                <field name="_ws.expert.message" showname="Message: Undecoded" hide="yes" size="0" pos="0" show="Undecoded"/>
                <field name="_ws.expert.severity" showname="Severity level: Note" size="0" pos="0" show="4194304"/>
                <field name="_ws.expert.group" showname="Group: Undecoded" size="0" pos="0" show="83886080"/>
              </field>
            </field>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));
        match parse_xml(&mut reader, &[]).unwrap() {
            Some(p) => match p.layer_name("btcommon") {
                Some(layer) => {
                    layer
                        .metadata("btcommon.eir_ad.entry.data")
                        .unwrap_or_else(|| panic!("Missing btcommon.eir_ad.entry.data"));

                    layer
                        .metadata("btcommon.eir_ad.undecoded")
                        .unwrap_or_else(|| panic!("Missing btcommon.eir_ad.undecoded"));
                }
                None => panic!("missing protocol"),
            },
            _ => panic!("invalid Output type"),
        }
    }

    #[test]
    fn test_rtshark_input_pcap() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder().input_path(pcap_path.to_str().unwrap());

        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        loop {
            match rtshark.read().unwrap() {
                None => break,
                Some(_) => todo!(),
            }
        }

        rtshark.kill();

        assert!(rtshark.pid().is_none());

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_input_pcap_decode_as() {
        // 0. prepare pcap
        let pcap = include_bytes!("rtp.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("rtp.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // 1. a first run without decode_as option

        // spawn tshark on it
        let builder = RTSharkBuilder::builder().input_path(pcap_path.to_str().unwrap());

        let mut rtshark = builder.spawn().unwrap();

        // read a packet, must be tcp without http2
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("rtp").is_none()),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        assert!(rtshark.pid().is_none());

        // 2. a second run with decode_as option
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .decode_as("udp.port==6000,rtp");

        let mut rtshark = builder.spawn().unwrap();

        // read a packet, must be http2
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("rtp").is_some()),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        assert!(rtshark.pid().is_none());

        // 3. cleanup
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_input_pcap_display_filter() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // first pass: get a udp packet
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .display_filter("udp.port == 53");

        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        // second pass: try a tcp packet
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .display_filter("tcp.port == 80");

        let mut rtshark = builder.spawn().unwrap();

        // we should get EOF since no packet is matching
        match rtshark.read().unwrap() {
            None => (),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_input_pcap_blacklist() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .metadata_blacklist("ip.src");
        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        let pkt = match rtshark.read().unwrap() {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        assert!(ip.metadata("ip.src").is_none());
        assert!(ip.metadata("ip.dst").unwrap().value().eq("127.0.0.1"));

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_input_pcap_whitelist() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .metadata_whitelist("ip.dst");
        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        let pkt = match rtshark.read().unwrap() {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        assert!(ip.metadata("ip.src").is_none());
        assert!(ip.metadata("ip.dst").unwrap().value().eq("127.0.0.1"));

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_input_pcap_multiple_whitelist() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .metadata_whitelist("ip.src")
            .metadata_whitelist("ip.dst");
        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        let pkt = match rtshark.read().unwrap() {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        assert!(ip.metadata("ip.src").unwrap().value().eq("127.0.0.1"));
        assert!(ip.metadata("ip.dst").unwrap().value().eq("127.0.0.1"));

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_input_pcap_whitelist_multiple_layer() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .metadata_whitelist("ip.src")
            .metadata_whitelist("udp.dstport");
        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        let pkt = match rtshark.read().unwrap() {
            Some(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        assert!(ip.metadata("ip.src").unwrap().value().eq("127.0.0.1"));
        let ip = pkt.layer_name("udp").unwrap();
        assert!(ip.metadata("udp.dstport").unwrap().value().eq("53"));

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    // this test may fail if executed in parallel with other tests. Run it with --test-threads=1 option.
    #[test]
    fn test_rtshark_input_pcap_whitelist_missing_attr() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .metadata_whitelist("nosuchproto.nosuchmetadata");
        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        let ret = rtshark.read();
        assert!(ret.is_err());

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_input_fifo() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap())
            .live_capture();
        let mut rtshark = builder.spawn().unwrap();

        // send packets in the fifo
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path)
            .expect("unable to open fifo");
        output.write_all(pcap).expect("unable to write in fifo");

        // get analysis
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        // stop tshark
        rtshark.kill();

        // verify tshark is stopped
        assert!(rtshark.pid().is_none());

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_input_multiple_fifo() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path1 = tmp_dir.path().join("pcap1.pipe");
        let fifo_path2 = tmp_dir.path().join("pcap2.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path1, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // create another fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path2, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path1.to_str().unwrap())
            .input_path(fifo_path2.to_str().unwrap())
            .live_capture();
        let mut rtshark = builder.spawn().unwrap();

        // send one packet in the fifo1
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path1)
            .expect("unable to open fifo");
        output.write_all(pcap).expect("unable to write in fifo");

        // send one packet in the fifo2
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path2)
            .expect("unable to open fifo");
        output.write_all(pcap).expect("unable to write in fifo");

        // get analysis from first packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        // get analysis for second packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        // stop tshark
        rtshark.kill();

        // verify tshark is stopped
        assert!(rtshark.pid().is_none());

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_input_pcap_filter_pcap() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // first, check with the right filter, we get the packet
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap())
            .live_capture()
            .capture_filter("port 53");

        let mut rtshark = builder.spawn().unwrap();

        // send packets in the fifo
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path)
            .expect("unable to open fifo");
        output.write_all(pcap).expect("unable to write in fifo");

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        assert!(rtshark.pid().is_none());

        // then, check with the bad filter, we don't get the packet
        // TODO (need a pcap with 2 packets, first will be filtered out)

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(all(target_family = "unix", not(target_os = "macos")))]
    #[test]
    fn test_rtshark_drop() {
        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap())
            .live_capture();

        let pid = {
            let rtshark = builder.spawn().unwrap();
            let pid = rtshark.pid().unwrap();

            assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
            pid
        };

        // verify tshark is stopped
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_killed() {
        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap())
            .live_capture();

        let mut rtshark = builder.spawn().unwrap();

        // killing badly
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(rtshark.pid().unwrap() as libc::pid_t),
            nix::sys::signal::Signal::SIGKILL,
        )
        .unwrap();

        // reading from process output should give EOF
        match rtshark.read().unwrap() {
            None => (),
            _ => panic!("invalid Output type"),
        }

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_fifo_lost() {
        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap())
            .live_capture();

        let mut rtshark = builder.spawn().unwrap();

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");

        // reading from process output should give 2 error messages then EOF
        loop {
            match rtshark.read() {
                Ok(e) if e.is_some() => panic!("invalid Output type"),
                Ok(e) if e.is_none() => break,
                _ => (),
            }
        }
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_fifo_opened_then_closed() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap())
            .live_capture();

        let mut rtshark = builder.spawn().unwrap();

        // send packets in the fifo then close it immediately
        {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .open(&fifo_path)
                .expect("unable to open fifo");
            output.write_all(pcap).expect("unable to write in fifo");
        }

        // get analysis
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        // disable this check for now - fails due to "normal" error message on stderr when tshark stops:
        // ---- tests::test_rtshark_fifo_opened_then_closed stdout ----
        // thread 'tests::test_rtshark_fifo_opened_then_closed' panicked at 'called `Result::unwrap()` on an `Err` value: Custom { kind: InvalidInput, error: "1 packet captured\n" }', src/lib.rs:1924:30
        // note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
        /*
        match rtshark.read().unwrap() {
            None => (),
            _ => panic!("invalid Output type"),
        }
        */

        // stop tshark
        rtshark.kill();

        // reading from process output should give EOF
        // disable this check for now - fails due to "normal" error message on stderr when tshark stops:
        // ---- tests::test_rtshark_fifo_opened_then_closed stdout ----
        // thread 'tests::test_rtshark_fifo_opened_then_closed' panicked at 'called `Result::unwrap()` on an `Err` value: Custom { kind: InvalidInput, error: "tshark: \n" }', src/lib.rs:1969:30
        // note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

        /*
        match rtshark.read().unwrap() {
            None => (),
            _ => panic!("invalid Output type"),
        }
        */

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_file_missing() {
        // start tshark on a missing fifo
        let builder = RTSharkBuilder::builder().input_path("/missing/rtshark/fifo");

        let ret = builder.spawn();

        match ret {
            Ok(_) => panic!("We can't start if file is missing"),
            Err(e) => eprintln!("{e}"),
        }
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_set_options() {
        let pcap = include_bytes!("tcp_fragmentation.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // second pass: turn on relative sequence numbers
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .option("tcp.relative_sequence_numbers:true");

        let mut rtshark = builder.spawn().unwrap();

        match rtshark.read().unwrap() {
            Some(p) => {
                let tcp = p.layer_name("tcp").expect("tcp layer");
                if !tcp.metadata.iter().any(|md| {
                    if let Some(display) = md.display() {
                        display.contains("relative sequence number")
                    } else {
                        false
                    }
                }) {
                    panic!("expected relative sequence number")
                }
            }
            e => panic!("invalid Output type: {:?}", e),
        }

        rtshark.kill();

        // second pass: turn off relative sequence numbers
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .option("tcp.relative_sequence_numbers:false");

        let mut rtshark = builder.spawn().unwrap();

        // we should not see any relative sequence numbers
        match rtshark.read().unwrap() {
            Some(p) => {
                let tcp = p.layer_name("tcp").expect("tcp layer");
                if tcp.metadata.iter().any(|md| {
                    if let Some(display) = md.display() {
                        display.contains("relative sequence number")
                    } else {
                        false
                    }
                }) {
                    panic!("expected no relative sequence numbers")
                }
            }
            e => panic!("invalid Output type: {:?}", e),
        }

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_set_disabled_protocols() {
        let pcap = include_bytes!("tcp_fragmentation.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // turn off tcp and sip protocols
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .disable_protocol("tcp")
            .disable_protocol("sip");

        let mut rtshark = builder.spawn().unwrap();

        match rtshark.read().unwrap() {
            Some(p) => {
                assert!(p.layer_name("tcp").is_none());
                assert!(p.layer_name("sip").is_none());
            }
            e => panic!("invalid Output type: {:?}", e),
        }

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_set_enabled_protocols() {
        let pcap = include_bytes!("tcp_fragmentation.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // turn off everything except eth and ip
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .disable_protocol("ALL")
            .enable_protocol("eth")
            .enable_protocol("ip");

        let mut rtshark = builder.spawn().unwrap();

        match rtshark.read().unwrap() {
            Some(p) => {
                assert!(p.layer_name("tcp").is_none());
                assert!(p.layer_name("sip").is_none());
                assert!(p.layer_name("ip").is_some());
            }
            e => panic!("invalid Output type: {:?}", e),
        }

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    #[serial] // Run test serially since its modifying env PATH
    fn test_rtshark_tshark_missing() {
        // clear PATH env (if tshark is already in PATH)
        let path = match std::env::var("PATH") {
            Ok(v) => {
                std::env::remove_var("PATH");
                Some(v)
            }
            Err(_) => None,
        };

        // start tshark on a missing fifo
        let builder = RTSharkBuilder::builder()
            .input_path("/missing/rtshark/fifo")
            .live_capture()
            .env_path("/invalid/path");

        let ret = builder.spawn();

        // restore PATH env (for other tests)
        if let Some(v) = path {
            std::env::set_var("PATH", v);
        }

        match ret {
            Ok(_) => panic!("We can't start if tshark is missing"),
            Err(e) => eprintln!("{e}"),
        }
    }

    #[test]
    fn test_rtshark_input_pcap_output_pcap() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let in_path = tmp_dir.path().join("in.pcap");
        let mut output = std::fs::File::create(&in_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        let out_path = tmp_dir.path().join("out.pcap");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(in_path.to_str().unwrap())
            .output_path(out_path.to_str().unwrap());

        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        loop {
            match rtshark.read().unwrap() {
                None => break,
                Some(_) => todo!(),
            }
        }

        rtshark.kill();

        assert!(rtshark.pid().is_none());

        // now check what was written
        let mut rtshark = RTSharkBuilder::builder()
            .input_path(out_path.to_str().unwrap())
            .spawn()
            .unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_rtshark_input_fifo_output_pcap() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        let out_path = tmp_dir.path().join("out.pcap");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap())
            .output_path(out_path.to_str().unwrap())
            .live_capture();
        let mut rtshark = builder.spawn().unwrap();

        // send packets in the fifo
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path)
            .expect("unable to open fifo");
        output.write_all(pcap).expect("unable to write in fifo");

        // get analysis
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        // stop tshark
        rtshark.kill();

        // verify tshark is stopped
        assert!(rtshark.pid().is_none());

        // now check what was written
        let mut rtshark = RTSharkBuilder::builder()
            .input_path(out_path.to_str().unwrap())
            .spawn()
            .unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }
    #[test]
    #[serial] // Run test serially to limit check to multiple spawns in test
    fn test_rtshark_multiple_spawn_pcap() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let in_path = tmp_dir.path().join("in.pcap");
        let mut output = std::fs::File::create(&in_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        let out_path = tmp_dir.path().join("out.pcap");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(in_path.to_str().unwrap())
            .output_path(out_path.to_str().unwrap());

        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        // retry
        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_timestamp_micros() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let in_path = tmp_dir.path().join("in.pcap");
        let mut output = std::fs::File::create(&in_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        let out_path = tmp_dir.path().join("out.pcap");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(in_path.to_str().unwrap())
            .output_path(out_path.to_str().unwrap());

        let mut rtshark = builder.spawn().unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Some(p) => assert_eq!(p.timestamp_micros(), Some(1652011560275852)),
            _ => panic!("invalid Output type"),
        }

        rtshark.kill();

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_tls_keylogfile_pcap() {
        let pcap = include_bytes!("test_tls.pcap");
        let keylog = include_bytes!("test_tlskeylogfile.txt");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder().input_path(pcap_path.to_str().unwrap());

        let mut rtshark = builder.spawn().unwrap();

        // read packets
        loop {
            match rtshark.read().unwrap() {
                None => break,
                Some(p) => {
                    // we check there is no visible http2
                    assert!(p.layer_name("tcp").is_some());
                    assert!(p.layer_name("http2").is_none())
                }
            }
        }

        rtshark.kill();

        let keylog_path = tmp_dir.path().join("keylogfile.txt");
        let mut output = std::fs::File::create(&keylog_path).expect("unable to open file");
        output.write_all(keylog).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            .keylog_file(keylog_path.as_os_str().to_str().unwrap());

        let mut rtshark = builder.spawn().unwrap();

        // read packets and search for http2 get
        let mut http2_found = false;
        loop {
            match rtshark.read().unwrap() {
                None => break,
                Some(p) => {
                    // we check there is a http2 method GET
                    assert!(p.layer_name("tcp").is_some());
                    if p.layer_name("http2").is_some() {
                        http2_found = true;
                    }
                }
            }
        }

        assert!(http2_found);

        rtshark.kill();

        assert!(rtshark.pid().is_none());
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_reassembled_tcp() {
        let pcap = include_bytes!("tcp_fragmentation.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let pcap_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&pcap_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // spawn tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(pcap_path.to_str().unwrap())
            // The ClientHello is the fragmented message
            .display_filter("tls.handshake.type == 1");

        let mut rtshark = builder.spawn().unwrap();

        // read packets
        loop {
            match rtshark.read().unwrap() {
                None => break,
                Some(p) => {
                    let tcp = p.layer_name("tcp").expect("Missing tcp layer");
                    tcp.metadata("tcp.reassembled.data")
                        .expect("Missing metadata");
                }
            }
        }

        rtshark.kill();
        assert!(rtshark.pid().is_none());
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_tshark_version() {
        let builder = RTSharkBuilder::builder();
        builder.version().expect("Error getting tshark version");
    }

    #[test]
    fn test_batch() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_pcap").unwrap();
        let original = tmp_dir.path().join("original.pcap");
        std::fs::write(&original, pcap).unwrap();

        let normalized = tmp_dir.path().join("normalized.pcap");

        // Spawn tshark on it to normalize the input.
        RTSharkBuilder::builder()
            .input_path(original.to_str().unwrap())
            .output_path(normalized.to_str().unwrap())
            .batch()
            .unwrap();
        assert!(
            !std::fs::read(&normalized).unwrap().is_empty(),
            "assumed normalization to produce some output, but it did not"
        );

        // Spawn tshark on the normalized PCAP to actually produce an "interesting" output.
        let output = tmp_dir.path().join("output.pcap");
        RTSharkBuilder::builder()
            .input_path(normalized.to_str().unwrap())
            .output_path(output.to_str().unwrap())
            .batch()
            .unwrap();

        // Validate that the output matches the normalized input.
        let normalized = std::fs::read(normalized).unwrap();
        let output = std::fs::read(output).unwrap();

        assert_eq!(normalized, output);
    }
}
