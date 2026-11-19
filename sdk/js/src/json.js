// A11: JSON.parse rrumbullakos çdo numër të plotë mbi 2^53 - 1 PARA se kodi ta shohë. Këtu numrat e
// plotë të pasigurt kthehen në tekst para parsimit (jashtë vargjeve), ndaj `toU64` / `BigInt` marrin
// shifrat origjinale. Numrat e sigurt dhe ata me pikë/eksponent mbeten siç janë.

/** @param {string} text */
export function parseJsonExact(text) {
  let out = "";
  let i = 0;
  const n = text.length;
  while (i < n) {
    const c = text[i];
    if (c === '"') {
      // Kopjo vargun të paprekur (me escape-t).
      let j = i + 1;
      while (j < n && text[j] !== '"') j += text[j] === "\\" ? 2 : 1;
      out += text.slice(i, j + 1);
      i = j + 1;
    } else if (c === "-" || (c >= "0" && c <= "9")) {
      let j = i + 1;
      while (j < n && /[0-9eE+\-.]/.test(text[j])) j++;
      const tok = text.slice(i, j);
      out += /^-?(0|[1-9][0-9]*)$/.test(tok) && !Number.isSafeInteger(Number(tok)) ? `"${tok}"` : tok;
      i = j;
    } else {
      out += c;
      i++;
    }
  }
  return JSON.parse(out);
}
