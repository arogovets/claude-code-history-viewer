"""Private control-plane worker shared by local and SSH transports (Python 3, POSIX).
Requests arrive on stdin, never in process arguments. No files are retained except
an origin-side lock and the final configuration. Raw data never enters the mirror.
"""
import base64
import fcntl
import hashlib
import json
import os
import signal
import stat
import sys
import uuid

LIMIT = 1024 * 1024
signal.alarm(15)


def directory(path, create=False):
    if not path.startswith('/') or any(p in ('.', '..') for p in path.split('/')):
        raise ValueError('unsafe_path')
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in filter(None, path.split('/')):
            if create:
                try:
                    os.mkdir(part, 0o700, dir_fd=fd)
                except FileExistsError:
                    pass
            nxt = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = nxt
        return fd
    except BaseException:
        os.close(fd)
        raise


def read_at(parent, name):
    try:
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    except FileNotFoundError:
        return None
    with os.fdopen(fd, 'rb') as file:
        if not stat.S_ISREG(os.fstat(file.fileno()).st_mode):
            raise ValueError('unsafe_path')
        data = file.read(LIMIT + 1)
        if len(data) > LIMIT:
            raise ValueError('file_too_large')
        return data


def revision(data):
    return 'missing' if data is None else hashlib.sha256(data).hexdigest()


def response(data):
    return {'revision': revision(data), 'bytes': None if data is None else base64.b64encode(data).decode()}


def run(req):
    # Host availability is checked independently of whether this scope exists.
    home = directory(req['home'])
    os.close(home)
    if req['operation'] == 'probe':
        return {}
    path = req['path']
    if not path.startswith('/') or any(p in ('', '.', '..') for p in path[1:].split('/')):
        raise ValueError('unsafe_path')
    parent_path, name = os.path.split(path)
    try:
        parent = directory(parent_path, req['operation'] == 'write')
    except FileNotFoundError:
        return response(None)
    try:
        if req['operation'] == 'read':
            return response(read_at(parent, name))
        if req['operation'] != 'write':
            raise ValueError('invalid_operation')
        lock = os.open('.' + name + '.cchv.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600, dir_fd=parent)
        with os.fdopen(lock, 'r+') as lockfile:
            if not stat.S_ISREG(os.fstat(lockfile.fileno()).st_mode):
                raise ValueError('unsafe_path')
            fcntl.flock(lockfile, fcntl.LOCK_EX)
            original = read_at(parent, name)
            if revision(original) != req['revision']:
                raise ValueError('revision_conflict')
            data = base64.b64decode(req['bytes'], validate=True)
            if len(data) > LIMIT:
                raise ValueError('file_too_large')
            temporary = '.' + name + '.cchv-' + uuid.uuid4().hex
            try:
                fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600, dir_fd=parent)
                with os.fdopen(fd, 'wb') as output:
                    if original is not None:
                        mode = stat.S_IMODE(os.stat(name, dir_fd=parent, follow_symlinks=False).st_mode)
                        os.fchmod(output.fileno(), mode & 0o777)
                    output.write(data)
                    output.flush()
                    os.fsync(output.fileno())
                # Check again immediately before the atomic replacement. External
                # writers need not participate in CCHV's serialization lock.
                if revision(read_at(parent, name)) != req['revision']:
                    raise ValueError('revision_conflict')
                os.replace(temporary, name, src_dir_fd=parent, dst_dir_fd=parent)
                os.fsync(parent)
                return response(read_at(parent, name))
            finally:
                try:
                    os.unlink(temporary, dir_fd=parent)
                except FileNotFoundError:
                    pass
    finally:
        os.close(parent)


try:
    request = json.loads(sys.stdin.buffer.read(3 * LIMIT))
    print(json.dumps({'ok': run(request)}))
except BaseException as error:
    # Never echo a request, config value, OS exception or traceback to the UI.
    code = str(error) if isinstance(error, ValueError) else 'origin_io_error'
    if code not in ('unsafe_path', 'file_too_large', 'revision_conflict', 'invalid_operation'):
        code = 'origin_io_error'
    print(json.dumps({'error': code}))
