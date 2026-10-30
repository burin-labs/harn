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
  };

  /** @typedef {keyof typeof catalogs} Locale */

  /** @param {string | null | undefined} value @returns {Locale | undefined} */
  function localeFor(value) {
    if (!value) {
      return undefined;
    }
    const normalized = value.replaceAll("_", "-").toLowerCase();
    const exact = Object.keys(catalogs).find(
      (locale) => locale.toLowerCase() === normalized,
    );
    if (exact) {
      return /** @type {Locale} */ (exact);
    }
    if (/^zh-(?:hant|tw|hk|mo)(?:-|$)/.test(normalized)) {
      return undefined;
    }
    if (normalized === "zh" || normalized.startsWith("zh-")) {
      return "zh-Hans";
    }
    if (normalized === "pt" || normalized.startsWith("pt-")) {
      return "pt-BR";
    }
    const base = normalized.split("-")[0];
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
