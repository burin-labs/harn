import {legacyConsumerObservationPolicy} from '../lib/consumer-observer-window.mjs';
console.log(JSON.stringify({...legacyConsumerObservationPolicy(),budget_version:1}));
