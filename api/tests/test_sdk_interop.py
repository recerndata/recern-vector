"""Cross-language, persisted data proof. No generated fixtures are rewritten."""
import json
import os
from pathlib import Path
import subprocess
import uuid
import shutil
import ssl
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
from recern_vector.portable import Client, export_snapshot, import_snapshot
from test_server import BIN, KEY, Server

ROOT=Path(__file__).resolve().parents[2]
RUST=os.getenv("RECERN_TEST_CLIENT_BIN")
NODE=os.getenv("RECERN_TEST_NODE_PORTABLE")


@pytest.mark.parametrize("backend", ["embedded", "remote", "qdrant"])
def test_python_rust_node_data_and_migration(tmp_path, backend):
    if not RUST or not NODE:
        pytest.skip("set RECERN_TEST_CLIENT_BIN and RECERN_TEST_NODE_PORTABLE to built SDK artifacts")
    server=None
    if backend == "embedded":
        path=tmp_path/"cross.rvec"
        client=Client.embedded(path)
        arguments=["--path",str(path)]
        config={"backend":"embedded","path":str(path)}
    elif backend == "remote":
        if not BIN:
            pytest.skip("set RECERN_TEST_SERVER_BIN")
        server=Server(tmp_path/"server")
        client=Client.remote(server.url,api_key=KEY)
        arguments=["--remote",server.url]
        config={"backend":"remote","url":server.url,"apiKey":KEY}
    else:
        url=os.getenv("RECERN_TEST_QDRANT_URL")
        if not url:
            pytest.skip("set RECERN_TEST_QDRANT_URL")
        key=os.getenv("RECERN_TEST_QDRANT_API_KEY")
        namespace="interop_"+uuid.uuid4().hex[:16]
        client=Client.qdrant(url,api_key=key,namespace=namespace)
        arguments=["--qdrant",url,"--namespace",namespace]
        config={"backend":"qdrant","url":url,"apiKey":key,"namespace":namespace}
    source=Client.embedded(tmp_path/"source.rvec")
    source.create_collection("interop",2).upsert("python/東京\n\0",[1,0],
        {"u64":2**64-1,"float":[1.0],"integer":[1]})
    export_snapshot(source,tmp_path/"archive")
    try:
        assert import_snapshot(tmp_path/"archive",client)["verified"]
        env={**os.environ,"RECERN_API_KEY":config.get("apiKey") or "",
             "RECERN_NODE_PORTABLE":str(Path(NODE).resolve())}
        result=subprocess.run([str(Path(RUST).resolve()),*arguments,"--check"],capture_output=True,text=True,env=env,timeout=30,check=True)
        assert json.loads(result.stdout)["verified"]
        result=subprocess.run(["node",str(ROOT/"tests/portable-interop.cjs")],input=json.dumps(config),capture_output=True,text=True,env=env,timeout=30,check=True)
        assert json.loads(result.stdout)["count"] == "3"
        # Embedded clients own their in-memory view, so reopen after other
        # writers. Remote/Qdrant reads observe their acknowledged writes.
        if backend == "embedded":
            client=Client.embedded(path,read_only=True)
        col=client.collection("interop")
        assert col.count() == 3
        for id in ("python/東京\n\0","rust","node"):
            value=col.get(id).metadata
            assert value["u64"] == 2**64-1 and type(value["float"][0]) is float and type(value["integer"][0]) is int
        if backend != "qdrant":
            export_snapshot(client,tmp_path/"roundtrip")
            restored=Client.embedded(tmp_path/"restored.rvec")
            assert import_snapshot(tmp_path/"roundtrip",restored)["verified"]
            assert restored.collection("interop").count() == 3
    finally:
        if backend == "qdrant":
            for name in client.list_collections():
                client.drop_collection(name)
        client.close()
        if server:
            server.stop()


def test_private_ca_is_verified_by_all_clients(tmp_path):
    if not RUST or not NODE or not BIN or not shutil.which("openssl"):
        pytest.skip("requires built SDKs, server and openssl")
    server=Server(tmp_path/"tls-server")
    with Client.remote(server.url,api_key=KEY) as source:
        source.create_collection("interop",2).upsert("python/東京\n\0",[1,0],
            {"u64":2**64-1,"float":[1.0],"integer":[1]})
    cert, key=tmp_path/"ca.pem",tmp_path/"server-key.pem"
    subprocess.run(["openssl","req","-x509","-newkey","rsa:2048","-nodes","-days","1",
        "-subj","/CN=Recern test CA","-addext","basicConstraints=critical,CA:TRUE",
        "-addext","keyUsage=critical,keyCertSign,cRLSign","-addext","subjectKeyIdentifier=hash",
        "-keyout",str(tmp_path/"ca-key.pem"),"-out",str(cert)],capture_output=True,check=True)
    subprocess.run(["openssl","req","-new","-newkey","rsa:2048","-nodes","-subj","/CN=localhost",
        "-keyout",str(key),"-out",str(tmp_path/"leaf.csr")],capture_output=True,check=True)
    extensions=tmp_path/"extensions.cnf"
    extensions.write_text("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n")
    subprocess.run(["openssl","x509","-req","-in",str(tmp_path/"leaf.csr"),"-CA",str(cert),
        "-CAkey",str(tmp_path/"ca-key.pem"),"-CAcreateserial","-days","1","-sha256",
        "-extfile",str(extensions),"-out",str(tmp_path/"server.pem")],capture_output=True,check=True)
    class Proxy(BaseHTTPRequestHandler):
        def dispatch(self):
            body=self.rfile.read(int(self.headers.get("Content-Length","0"))) if self.command == "POST" else None
            headers={name:self.headers[name] for name in ("Content-Type","Authorization","X-Recern-Timeout-Ms") if name in self.headers}
            request=urllib.request.Request(server.url+self.path,data=body,headers=headers,method=self.command)
            with urllib.request.urlopen(request,timeout=10) as response:
                raw=response.read()
                self.send_response(response.status)
                for name in ("Content-Type","X-Request-ID","Cache-Control"):
                    self.send_header(name,response.headers[name])
                self.send_header("Content-Length",str(len(raw))); self.end_headers()
                self.wfile.write(raw)
        do_GET=do_POST=dispatch
        def log_message(self,*args):
            pass
    http=ThreadingHTTPServer(("127.0.0.1",0),Proxy)
    context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); context.load_cert_chain(tmp_path/"server.pem",key)
    http.socket=context.wrap_socket(http.socket,server_side=True)
    thread=threading.Thread(target=http.serve_forever,daemon=True); thread.start()
    url=f"https://localhost:{http.server_port}"
    env={**os.environ,"RECERN_API_KEY":KEY,"RECERN_NODE_PORTABLE":str(Path(NODE).resolve())}
    try:
        from recern_vector.portable import Unavailable
        with pytest.raises(Unavailable):
            Client.remote(url,api_key=KEY,timeout=2)
        with Client.remote(url,api_key=KEY,ca_file=str(cert)) as client:
            assert client.collection("interop").count() == 1
        untrusted=subprocess.run([RUST,"--remote",url,"--check"],capture_output=True,text=True,env=env,timeout=15)
        assert untrusted.returncode != 0 and "Unavailable" in untrusted.stderr and KEY not in untrusted.stderr
        result=subprocess.run([RUST,"--remote",url,"--ca",str(cert),"--check"],capture_output=True,text=True,env=env,timeout=30,check=True)
        assert json.loads(result.stdout)["verified"]
        config={"backend":"remote","url":url,"apiKey":KEY,"caFile":str(cert)}
        untrusted_script="const {Client}=require(process.env.RECERN_NODE_PORTABLE);const c=JSON.parse(require('node:fs').readFileSync(0,'utf8'));Client.remote(c.url,{apiKey:c.apiKey}).then(()=>{throw Error('untrusted CA accepted');},e=>{if(e.code!=='Unavailable')throw e;});"
        subprocess.run(["node","-e",untrusted_script],input=json.dumps(config),capture_output=True,text=True,env=env,timeout=20,check=True)
        result=subprocess.run(["node",str(ROOT/"tests/portable-interop.cjs")],input=json.dumps(config),capture_output=True,text=True,env=env,timeout=30,check=True)
        assert json.loads(result.stdout)["count"] == "3"
    finally:
        http.shutdown(); http.server_close(); thread.join(); server.stop()
