"""Execute only host-captured Python runner material in an isolated child."""
import importlib.abc
import importlib.util
import json
import os
import sys
import types

descriptor = int(sys.argv[1])
metadata = json.loads(sys.argv[2])
material = os.pread(descriptor, os.fstat(descriptor).st_size, 0)
os.close(descriptor)
sys.argv = [metadata['origin'], *metadata['args']]
witness_fd = metadata['witness_fd']
witness = bytes(metadata['witness'])
write_witness = os.pwrite
close_witness = os.close


class CapturedLoader(importlib.abc.MetaPathFinder, importlib.abc.Loader):
    def __init__(self, bundle):
        self.modules = bundle['modules']
        self.tops = {name.split('.')[0] for name in self.modules}
        if self.tops.intersection(sys.modules):
            raise ImportError('Verifier modules overlap the trusted bootstrap')

    def find_spec(self, fullname, path=None, target=None):
        if fullname in self.modules:
            item = self.modules[fullname]
            return importlib.util.spec_from_loader(
                fullname, self, origin=item['origin'], is_package=item['package'])
        if fullname.split('.')[0] in self.tops:
            raise ModuleNotFoundError('Module outside captured verifier: ' + fullname)
        return None

    def create_module(self, spec):
        return None

    def get_code(self, fullname):
        item = self.modules[fullname]
        return compile(bytes(item['source']), item['origin'], 'exec')

    def get_filename(self, fullname):
        return self.modules[fullname]['origin']

    def is_package(self, fullname):
        return self.modules[fullname]['package']

    def exec_module(self, module):
        module.__file__ = self.get_filename(module.__name__)
        exec(self.get_code(module.__name__), module.__dict__)


entry_reached = False


def execute():
    global entry_reached
    if metadata['module'] is not None:
        bundle = json.loads(material)
        loader = CapturedLoader(bundle)
        sys.meta_path.insert(0, loader)
        entry = bundle['entry']
        if loader.is_package(entry):
            importlib.import_module(entry)
            entry += '.__main__'
        elif '.' in entry:
            importlib.import_module(entry.rpartition('.')[0])
        origin = loader.get_filename(entry)
        spec = importlib.util.spec_from_loader(entry, loader, origin=origin)
        package = entry.rpartition('.')[0]
        compiled = loader.get_code(entry)
        sys.argv[0] = origin
    else:
        origin = metadata['origin']
        spec = None
        package = None
        compiled = compile(material, origin, 'exec')
    main_module = types.ModuleType('__main__')
    namespace = main_module.__dict__
    namespace.update({'__file__': origin, '__package__': package,
                      '__spec__': spec, '__cached__': None,
                      '__loader__': None if spec is None else spec.loader})
    sys.modules['__main__'] = main_module
    entry_reached = True
    exec(compiled, namespace)

try:
    execute()
finally:
    if entry_reached:
        write_witness(witness_fd, witness, 0)
    close_witness(witness_fd)
