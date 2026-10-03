import Foundation

/// The moment a report link points to: the fragment of `<a href="#t=754.0">`, in seconds from the
/// start of the recording. Nil for anything else, so the report's ordinary in-page anchors still scroll.
/// The report version a link opens: another `<id>.html` page in the same `reports` folder as the
/// page showing (the history's version links). Nil for anything else.
public func reportID(fromLink link: URL, currentPage: URL) -> Int? {
    guard link.isFileURL, currentPage.isFileURL, link.pathExtension == "html",
          link.deletingLastPathComponent().standardizedFileURL.path == currentPage.deletingLastPathComponent().standardizedFileURL.path,
          link.standardizedFileURL.path != currentPage.standardizedFileURL.path
    else { return nil }
    let name = link.deletingPathExtension().lastPathComponent
    guard !name.isEmpty, name.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
    return Int(name)
}

public func seekSeconds(fromFragment fragment: String?) -> Double? {
    guard var text = fragment?.trimmingCharacters(in: .whitespaces) else { return nil }
    if text.hasPrefix("#") { text.removeFirst() }
    let parts = text.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
    guard parts.count == 2, parts[0].trimmingCharacters(in: .whitespaces) == "t" else { return nil }
    let value = parts[1].trimmingCharacters(in: .whitespaces)
    // Plain decimals only: Double(_:) would also take a sign, an exponent, hex, "inf" and "nan".
    guard value.allSatisfy({ $0.isASCII && ($0.isNumber || $0 == ".") }),
          let seconds = Double(value), seconds.isFinite else { return nil }
    return seconds
}
