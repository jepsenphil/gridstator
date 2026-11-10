// Reference-fixture processes only: control a variable before Commander wraps help text.
const os = require('node:os');
const { syncBuiltinESMExports } = require('node:module');
os.hostname = () => 'fixture-host';
syncBuiltinESMExports();
