import { useEffect, useState } from "preact/hooks";
import { settingsCheckCity, settingsCurrencies, type SettingsState } from "./api";
import { SEARCH_ENGINES, TEMPERATURE, engineChoice, engineError } from "./choices";
import { useSaver } from "./saver";

export function Answers({ state }: { state: SettingsState }) {
  const { config } = state;
  const { errors, save } = useSaver(state);
  const [currencies, setCurrencies] = useState<string[]>([]);
  const [city, setCity] = useState(config.weather_city);
  const [cityNote, setCityNote] = useState<{ ok: boolean; text: string } | null>(null);
  const [custom, setCustom] = useState(false);
  const [engine, setEngine] = useState(config.search_engine);

  useEffect(() => {
    settingsCurrencies().then(setCurrencies, () => {});
  }, []);
  // The file is the truth: follow it after every reload.
  useEffect(() => {
    setCity(config.weather_city);
    setEngine(config.search_engine);
    setCustom((c) => c || engineChoice(config.search_engine) === "Custom");
  }, [state]);

  const checkCity = async () => {
    const wanted = city.trim();
    if (wanted === config.weather_city) return;
    if (wanted === "") {
      setCityNote(null);
      void save("weather_city", { key: "weather_city", value: "" });
      return;
    }
    // Saved only when the lookup finds the city, or unchecked while online is off.
    // A miss keeps the typed text and shows the error under it.
    if (!config.online) {
      setCityNote(null);
      void save("weather_city", { key: "weather_city", value: wanted });
      return;
    }
    try {
      const label = await settingsCheckCity(wanted);
      setCityNote({ ok: true, text: label });
      void save("weather_city", { key: "weather_city", value: wanted });
    } catch (e) {
      setCityNote({ ok: false, text: String(e) });
    }
  };

  const choice = custom ? "Custom" : engineChoice(config.search_engine);
  const pickEngine = (label: string) => {
    if (label === "Custom") {
      setCustom(true);
      return;
    }
    setCustom(false);
    const url = SEARCH_ENGINES.find((e) => e.label === label)?.url ?? "";
    void save("search_engine", { key: "search_engine", value: url });
  };
  const customError = choice === "Custom" && engine !== "" ? engineError(engine) : null;

  return (
    <div>
      <div class="field">
        <label class="check">
          <input
            type="checkbox"
            checked={config.online}
            onChange={(e) => void save("online", { key: "online", value: e.currentTarget.checked })}
          />
          Go online for currency and weather
        </label>
        {errors.online && <p class="error">{errors.online}</p>}
      </div>

      <div class="field">
        <label for="city">Home city for weather</label>
        <input
          id="city"
          type="text"
          value={city}
          onInput={(e) => setCity(e.currentTarget.value)}
          onBlur={checkCity}
          onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
        />
        {cityNote && <p class={cityNote.ok ? "ok" : "error"}>{cityNote.text}</p>}
        {errors.weather_city && <p class="error">{errors.weather_city}</p>}
      </div>

      <div class="field">
        <label for="currency">Currency</label>
        <select
          id="currency"
          value={config.currency}
          onChange={(e) => void save("currency", { key: "currency", value: e.currentTarget.value })}
        >
          <option value="">From Windows region</option>
          {currencies.map((c) => (
            <option key={c} value={c}>
              {c}
            </option>
          ))}
        </select>
        {errors.currency && <p class="error">{errors.currency}</p>}
      </div>

      <div class="field">
        <label for="temperature">Temperature</label>
        <select
          id="temperature"
          value={config.temperature_unit}
          onChange={(e) => void save("temperature_unit", { key: "temperature_unit", value: e.currentTarget.value })}
        >
          {TEMPERATURE.map(([value, label]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
        {errors.temperature_unit && <p class="error">{errors.temperature_unit}</p>}
      </div>

      <div class="field">
        <label for="engine">Search engine</label>
        <select id="engine" value={choice} onChange={(e) => pickEngine(e.currentTarget.value)}>
          {SEARCH_ENGINES.map((e) => (
            <option key={e.label} value={e.label}>
              {e.label}
            </option>
          ))}
          <option value="Custom">Custom</option>
        </select>
        {choice === "Custom" && (
          <>
            <input
              type="text"
              aria-label="Custom search address"
              placeholder="https://example.com/?q={arg}"
              value={engine}
              onInput={(e) => setEngine(e.currentTarget.value)}
              onBlur={() => {
                if (engine !== config.search_engine && engineError(engine) === null) {
                  void save("search_engine", { key: "search_engine", value: engine });
                }
              }}
            />
            <p class="hint">The address must start with https:// and contain {"{arg}"} where the search text goes.</p>
          </>
        )}
        {customError && <p class="error">{customError}</p>}
        {errors.search_engine && <p class="error">{errors.search_engine}</p>}
      </div>
    </div>
  );
}
