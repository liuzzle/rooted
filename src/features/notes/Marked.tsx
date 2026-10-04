import { ReactNode } from "react";

/**
 * A quoted passage with ranges marked.
 *
 * Split at the offsets it is given — never by searching the text again, since
 * the offsets are the record of where something was found, and marking another
 * occurrence would quietly misreport the evidence. Offsets are in characters
 * (code points), as the worker and the index record them.
 */
export default function Marked({
  text,
  marks,
}: {
  text: string;
  marks: [number, number][];
}) {
  const chars = Array.from(text);
  const parts: ReactNode[] = [];
  let at = 0;
  marks.forEach(([start, end], i) => {
    if (start < at) return;
    parts.push(chars.slice(at, start).join(""));
    parts.push(<mark key={i}>{chars.slice(start, end).join("")}</mark>);
    at = end;
  });
  parts.push(chars.slice(at).join(""));
  return <blockquote className="citation-text">{parts}</blockquote>;
}
