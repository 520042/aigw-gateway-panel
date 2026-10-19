import sys, time
t0 = time.time()
sys.path.insert(0, '.')
print('python in:', sys.version.split()[0], flush=True)

print('[%.1fs] import main ...' % (time.time()-t0), flush=True)
t1 = time.time()
import main
print('[%.1fs] import main done (took %.1fs)' % (time.time()-t0, time.time()-t1), flush=True)

print('[%.1fs] init_app ...' % (time.time()-t0), flush=True)
t2 = time.time()
main.init_app()
print('[%.1fs] init_app done (took %.1fs)' % (time.time()-t0, time.time()-t2), flush=True)

print('[%.1fs] load_route_config ...' % (time.time()-t0), flush=True)
t3 = time.time()
main.load_route_config(main.APP['router'], main.APP['store'], main.new_client())
print('[%.1fs] load_route_config done (took %.1fs)' % (time.time()-t0, time.time()-t3), flush=True)

print('[%.1fs] models in router:', list(main.APP['router'].list_models().keys()), flush=True)