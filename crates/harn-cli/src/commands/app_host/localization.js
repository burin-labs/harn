// @ts-check
/* exported HarnAppLocalization */

const HarnAppLocalization = (() => {
  const catalogs = {
    en: {
      app: "Harn App",
      sandbox: "Harn app sandbox",
      content: "Harn app content",
      starting: "starting",
      ready: "ready",
      connected: "connected",
    },
    es: {
      app: "Aplicación Harn",
      sandbox: "Entorno aislado de la aplicación Harn",
      content: "Contenido de la aplicación Harn",
      starting: "Iniciando",
      ready: "Lista",
      connected: "Conectada",
    },
    fr: {
      app: "Application Harn",
      sandbox: "Bac à sable de l’application Harn",
      content: "Contenu de l’application Harn",
      starting: "Démarrage",
      ready: "Prête",
      connected: "Connectée",
    },
    de: {
      app: "Harn Anwendung",
      sandbox: "Isolierte Harn Anwendung",
      content: "Inhalt der Harn Anwendung",
      starting: "Wird gestartet",
      ready: "Bereit",
      connected: "Verbunden",
    },
    "pt-BR": {
      app: "Aplicativo Harn",
      sandbox: "Ambiente isolado do aplicativo Harn",
      content: "Conteúdo do aplicativo Harn",
      starting: "Iniciando",
      ready: "Pronto",
      connected: "Conectado",
    },
    it: {
      app: "Applicazione Harn",
      sandbox: "Ambiente isolato dell’applicazione Harn",
      content: "Contenuto dell’applicazione Harn",
      starting: "Avvio in corso",
      ready: "Pronta",
      connected: "Connessa",
    },
    ja: {
      app: "Harn アプリ",
      sandbox: "Harn アプリのサンドボックス",
      content: "Harn アプリの内容",
      starting: "起動中",
      ready: "準備完了",
      connected: "接続済み",
    },
    ko: {
      app: "Harn 앱",
      sandbox: "Harn 앱 샌드박스",
      content: "Harn 앱 콘텐츠",
      starting: "시작 중",
      ready: "준비됨",
      connected: "연결됨",
    },
    "zh-Hans": {
      app: "Harn 应用",
      sandbox: "Harn 应用沙盒",
      content: "Harn 应用内容",
      starting: "正在启动",
      ready: "已就绪",
      connected: "已连接",
    },
    ru: {
      app: "Приложение Harn",
      sandbox: "Изолированная среда приложения Harn",
      content: "Содержимое приложения Harn",
      starting: "Запуск",
      ready: "Готово",
      connected: "Подключено",
    },
  };

  /** @typedef {keyof typeof catalogs} Locale */

  /** @param {string | null | undefined} value @returns {Locale | undefined} */
  function localeFor(value) {
    if (!value) {
      return undefined;
    }
    let parsed;
    try {
      parsed = new Intl.Locale(value.replaceAll("_", "-"));
    } catch {
      return undefined;
    }
    const base = parsed.language;
    if (base === "zh") {
      if (parsed.script) {
        return parsed.script === "Hans" ? "zh-Hans" : undefined;
      }
      return ["TW", "HK", "MO"].includes(parsed.region ?? "")
        ? undefined
        : "zh-Hans";
    }
    if (base === "pt") {
      return "pt-BR";
    }
    return Object.hasOwn(catalogs, base)
      ? /** @type {Locale} */ (base)
      : undefined;
  }

  /** @param {readonly (string | null | undefined)[]} candidates */
  function resolve(candidates) {
    const locale = candidates.map(localeFor).find(Boolean) ?? "en";
    return { locale, copy: catalogs[locale] };
  }

  return { resolve, supportedLocales: Object.keys(catalogs) };
})();
