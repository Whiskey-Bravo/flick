import { useEffect, useRef, useState } from "preact/hooks";
import { builtinName, serverUrl } from "../icons";
import { settingsSearchItems, type PickRow } from "./api";

const MAX_ROWS = 20;

function RowIcon({ icon }: { icon: string }) {
  if (icon === "" || builtinName(icon) !== null) return <span class="icon" />;
  return (
    <img
      class="icon"
      src={serverUrl(icon, window.devicePixelRatio || 1)}
      alt=""
      onError={(e) => {
        e.currentTarget.style.visibility = "hidden";
      }}
    />
  );
}

/** A search box that asks the backend which items match, as the bar would. */
export function ItemPicker({ forWorkspace, onPick }: { forWorkspace: boolean; onPick: (row: PickRow) => void }) {
  const [text, setText] = useState("");
  const [rows, setRows] = useState<PickRow[]>([]);
  const [sel, setSel] = useState(0);

  useEffect(() => {
    if (text.trim() === "") {
      setRows([]);
      return;
    }
    let stale = false;
    const t = setTimeout(() => {
      settingsSearchItems(text, forWorkspace).then(
        (found) => {
          if (stale) return;
          setRows(found.slice(0, MAX_ROWS));
          setSel(0);
        },
        () => !stale && setRows([]),
      );
    }, 150);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [text, forWorkspace]);

  const pick = (row: PickRow) => {
    onPick(row);
    setText("");
    setRows([]);
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "ArrowDown" && rows.length > 0) {
      e.preventDefault();
      setSel((s) => Math.min(s + 1, rows.length - 1));
    } else if (e.key === "ArrowUp" && rows.length > 0) {
      e.preventDefault();
      setSel((s) => Math.max(s - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (rows[sel]) pick(rows[sel]);
    }
  };

  const listRef = useRef<HTMLUListElement>(null);
  useEffect(() => {
    listRef.current?.children[sel]?.scrollIntoView({ block: "nearest" });
  }, [sel, rows]);

  return (
    <div class="ip">
      <input
        type="text"
        value={text}
        placeholder="Search apps, files, games, bookmarks…"
        onInput={(e) => setText(e.currentTarget.value)}
        onKeyDown={onKey}
      />
      {rows.length > 0 && (
        <ul class="ip-rows" ref={listRef}>
          {rows.map((r, i) => (
            <li key={r.provider + "\u0000" + r.id}>
              <button
                class={i === sel ? "ip-row active" : "ip-row"}
                onMouseEnter={() => setSel(i)}
                onClick={() => pick(r)}
              >
                <RowIcon icon={r.icon} />
                <span class="ip-title">{r.title}</span>
                <span class="ip-sub">{r.subtitle}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
