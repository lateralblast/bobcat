// bobcat: host for HPE iLO 4 Java console applet (com.hp.ilo2.intgapp.intgapp)
// Version: 2.0.2
// License: CC BY-NC-SA 4.0 (see LICENSE.md)
//
// The iLO serves its Java console as an applet; Web Start is gone from current Java, so this
// small program plays the browser: it supplies the applet's parameters and context and shows its
// window. Started by java_console.rs as `java -cp intgapp4_NNN.jar:ilohost.jar IloHost`.
//
// stdin (read once, so the session key is not on the command line):
//   line 1: the iLO's address
//   line 2: SHA-256 fingerprint (hex) of the iLO's pinned certificate; no other certificate is accepted
//   then `name=value` lines: the applet's parameters (from the iLO's own JNLP template)
//
// Build (needs a JDK, the result is checked in): ./build.sh in this folder.

import java.applet.Applet;
import java.applet.AppletContext;
import java.applet.AppletStub;
import java.awt.Toolkit;
import java.awt.event.WindowAdapter;
import java.awt.event.WindowEvent;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.lang.reflect.Proxy;
import java.net.URL;
import java.security.MessageDigest;
import java.security.cert.CertificateException;
import java.security.cert.X509Certificate;
import java.util.Collections;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.atomic.AtomicBoolean;
import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLEngine;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509ExtendedTrustManager;

public class IloHost implements AppletStub {
    private final Map<String, String> params = new HashMap<String, String>();
    private final URL base;
    private Applet applet;
    private final AtomicBoolean closed = new AtomicBoolean();

    private IloHost(URL base) {
        this.base = base;
    }

    public boolean isActive() {
        return true;
    }

    public URL getDocumentBase() {
        return base;
    }

    public URL getCodeBase() {
        return base;
    }

    public String getParameter(String name) {
        return params.get(name);
    }

    public void appletResize(int w, int h) {
    }

    public AppletContext getAppletContext() {
        return (AppletContext) Proxy.newProxyInstance(IloHost.class.getClassLoader(), new Class<?>[] { AppletContext.class }, (p, m, a) -> {
            switch (m.getName()) {
                case "getImage":
                    return Toolkit.getDefaultToolkit().getImage((URL) a[0]);
                case "getApplets":
                    return Collections.enumeration(Collections.emptyList());
                default:
                    return null;
            }
        });
    }

    /** Let the applet end its session (it sends the close messages), once. */
    private void close() {
        if (closed.compareAndSet(false, true)) {
            try {
                applet.stop();
                applet.destroy();
            } catch (Throwable t) {
                System.err.println("[ilohost] close: " + t);
            }
        }
    }

    private static void trustOnly(final String fingerprint) throws Exception {
        TrustManager[] pinned = { new X509ExtendedTrustManager() {
            private void check(X509Certificate[] chain) throws CertificateException {
                try {
                    byte[] d = MessageDigest.getInstance("SHA-256").digest(chain[0].getEncoded());
                    StringBuilder hex = new StringBuilder();
                    for (byte b : d) {
                        hex.append(String.format("%02x", b));
                    }
                    if (!hex.toString().equalsIgnoreCase(fingerprint)) {
                        throw new CertificateException("the iLO's certificate is not the pinned one");
                    }
                } catch (java.security.GeneralSecurityException e) {
                    throw new CertificateException(e);
                }
            }

            public void checkClientTrusted(X509Certificate[] c, String a) throws CertificateException {
                throw new CertificateException("no client certificates");
            }

            public void checkServerTrusted(X509Certificate[] c, String a) throws CertificateException {
                check(c);
            }

            public void checkClientTrusted(X509Certificate[] c, String a, java.net.Socket s) throws CertificateException {
                throw new CertificateException("no client certificates");
            }

            public void checkServerTrusted(X509Certificate[] c, String a, java.net.Socket s) throws CertificateException {
                check(c);
            }

            public void checkClientTrusted(X509Certificate[] c, String a, SSLEngine e) throws CertificateException {
                throw new CertificateException("no client certificates");
            }

            public void checkServerTrusted(X509Certificate[] c, String a, SSLEngine e) throws CertificateException {
                check(c);
            }

            public X509Certificate[] getAcceptedIssuers() {
                return new X509Certificate[0];
            }
        } };
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(null, pinned, null);
        SSLContext.setDefault(ctx);
        HttpsURLConnection.setDefaultSSLSocketFactory(ctx.getSocketFactory());
        // the certificate is pinned by fingerprint, its name does not matter (iLOs are reached by address)
        HttpsURLConnection.setDefaultHostnameVerifier((h, s) -> true);
    }

    public static void main(String[] args) throws Exception {
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, "UTF-8"));
        String ip = in.readLine();
        String fingerprint = in.readLine();
        if (ip == null || fingerprint == null) {
            System.err.println("[ilohost] usage: address, fingerprint and name=value lines on stdin");
            System.exit(2);
        }
        trustOnly(fingerprint.trim());
        final IloHost host = new IloHost(new URL("https://" + ip.trim() + "/html/"));
        String line;
        while ((line = in.readLine()) != null) {
            int eq = line.indexOf('=');
            if (eq > 0) {
                host.params.put(line.substring(0, eq), line.substring(eq + 1));
            }
        }
        host.applet = (Applet) Class.forName("com.hp.ilo2.intgapp.intgapp").getDeclaredConstructor().newInstance();
        host.applet.setStub(host);
        // SIGTERM (bobcat's Disconnect): close the console session properly
        Runtime.getRuntime().addShutdownHook(new Thread(host::close));
        host.applet.init();
        // the applet's window: its own close handler only calls stop(), so end the program here
        try {
            java.awt.Window w = (java.awt.Window) host.applet.getClass().getField("dispFrame").get(host.applet);
            if (w != null) {
                w.addWindowListener(new WindowAdapter() {
                    public void windowClosing(WindowEvent e) {
                        host.close();
                        System.exit(0);
                    }
                });
            }
        } catch (Throwable t) {
            System.err.println("[ilohost] could not watch the console window: " + t);
        }
        host.applet.start();
        Thread.sleep(Long.MAX_VALUE);
    }
}
