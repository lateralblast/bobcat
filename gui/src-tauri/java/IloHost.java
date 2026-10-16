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
import java.awt.Color;
import java.awt.Component;
import java.awt.Dimension;
import java.awt.Graphics;
import java.awt.Graphics2D;
import java.awt.RenderingHints;
import java.awt.Toolkit;
import java.awt.event.MouseAdapter;
import java.awt.event.MouseEvent;
import java.awt.event.MouseListener;
import java.awt.event.MouseMotionListener;
import java.awt.event.MouseWheelEvent;
import java.awt.event.MouseWheelListener;
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
import javax.swing.JComponent;
import javax.swing.JLayer;
import javax.swing.JScrollPane;
import javax.swing.Scrollable;
import javax.swing.SwingUtilities;
import javax.swing.plaf.LayerUI;
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
    /** The picture's scale from bobcat's size selector: 0 = fit the window. */
    private static volatile double zoom = 0;
    private static volatile Runnable zoomChanged;

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


    /**
     * What the layer sits in. "Fit" (factor 0): takes the whole viewport. A fixed factor: the picture's
     * size times the factor, with scroll bars when that is bigger than the window.
     */
    private static class Fill extends javax.swing.JPanel implements Scrollable {
        private final ScaleUI ui;

        Fill(JLayer<Component> layer, ScaleUI ui) {
            super(new java.awt.BorderLayout());
            this.ui = ui;
            add(layer, java.awt.BorderLayout.CENTER);
        }

        @Override
        public Dimension getPreferredSize() {
            Dimension n = ui.factor > 0 ? ui.nativeSize() : null;
            return n == null ? new Dimension(64, 64)
                : new Dimension((int) Math.ceil(n.width * ui.factor), (int) Math.ceil(n.height * ui.factor));
        }

        public Dimension getPreferredScrollableViewportSize() {
            return new Dimension(64, 64);
        }

        public int getScrollableUnitIncrement(java.awt.Rectangle r, int o, int d) {
            return 20;
        }

        public int getScrollableBlockIncrement(java.awt.Rectangle r, int o, int d) {
            return 200;
        }

        public boolean getScrollableTracksViewportWidth() {
            return ui.factor <= 0 || getParent().getWidth() > getPreferredSize().width;
        }

        public boolean getScrollableTracksViewportHeight() {
            return ui.factor <= 0 || getParent().getHeight() > getPreferredSize().height;
        }
    }

    /**
     * Draws the console picture scaled to the window (the applet only shows it at 100% with scroll
     * bars) and maps the mouse back to the picture's own pixels. The picture component keeps its
     * native size; only its painting and the mouse events are transformed.
     */
    private static class ScaleUI extends LayerUI<Component> {
        private final Component view;
        /** 0: fit the window; -1: stretch to the window; otherwise the picture's scale (1 = its own pixels). */
        volatile double factor = zoom;

        ScaleUI(Component view) {
            this.view = view;
        }

        Dimension nativeSize() {
            Dimension d = view.getPreferredSize();
            return (d == null || d.width < 1 || d.height < 1) ? null : d;
        }

        private double scaleX(JComponent l) {
            Dimension n = nativeSize();
            if (n == null) {
                return 1;
            }
            if (factor > 0) {
                return factor;
            }
            if (factor < 0) {
                return (double) l.getWidth() / n.width; // stretch: each axis on its own
            }
            return Math.min((double) l.getWidth() / n.width, (double) l.getHeight() / n.height);
        }

        private double scaleY(JComponent l) {
            Dimension n = nativeSize();
            if (n == null) {
                return 1;
            }
            if (factor < 0) {
                return (double) l.getHeight() / n.height;
            }
            return scaleX(l);
        }

        private double offX(JComponent l) {
            Dimension n = nativeSize();
            return n == null ? 0 : Math.max(0, (l.getWidth() - n.width * scaleX(l)) / 2);
        }

        private double offY(JComponent l) {
            Dimension n = nativeSize();
            return n == null ? 0 : Math.max(0, (l.getHeight() - n.height * scaleY(l)) / 2);
        }

        @Override
        public void doLayout(JLayer<? extends Component> l) {
            Dimension n = nativeSize();
            if (n != null) {
                view.setBounds(0, 0, n.width, n.height);
            }
        }

        @Override
        public void paint(Graphics g, JComponent c) {
            double sx = scaleX(c), sy = scaleY(c);
            Graphics2D g2 = (Graphics2D) g.create();
            g2.setColor(Color.BLACK);
            g2.fillRect(0, 0, c.getWidth(), c.getHeight());
            g2.translate(offX(c), offY(c));
            g2.scale(sx, sy);
            g2.setRenderingHint(RenderingHints.KEY_INTERPOLATION, RenderingHints.VALUE_INTERPOLATION_BILINEAR);
            g2.setClip(0, 0, view.getWidth(), view.getHeight());
            super.paint(g2, c);
            g2.dispose();
        }

        @Override
        public void paintImmediately(int x, int y, int w, int h, JLayer<? extends Component> l) {
            // the picture reports its changes in its own pixels
            double sx = scaleX(l), sy = scaleY(l);
            int ox = (int) offX(l);
            int oy = (int) offY(l);
            super.paintImmediately((int) Math.floor(x * sx) + ox - 1, (int) Math.floor(y * sy) + oy - 1,
                (int) Math.ceil(w * sx) + 3, (int) Math.ceil(h * sy) + 3, l);
        }

        /** Mouse events on the layer, in the picture's pixels. */
        MouseEvent map(MouseEvent e, JComponent l) {
            int x = (int) Math.round((e.getX() - offX(l)) / scaleX(l));
            int y = (int) Math.round((e.getY() - offY(l)) / scaleY(l));
            x = Math.max(0, Math.min(view.getWidth() - 1, x));
            y = Math.max(0, Math.min(view.getHeight() - 1, y));
            if (e instanceof MouseWheelEvent) {
                MouseWheelEvent w = (MouseWheelEvent) e;
                return new MouseWheelEvent(view, e.getID(), e.getWhen(), e.getModifiersEx(), x, y, e.getXOnScreen(),
                    e.getYOnScreen(), e.getClickCount(), e.isPopupTrigger(), w.getScrollType(), w.getScrollAmount(),
                    w.getWheelRotation());
            }
            return new MouseEvent(view, e.getID(), e.getWhen(), e.getModifiersEx(), x, y, e.getXOnScreen(),
                e.getYOnScreen(), e.getClickCount(), e.isPopupTrigger(), e.getButton());
        }
    }

    /**
     * Scale the console picture to the window. The applet puts the picture (remconsObj.session)
     * in a scroll pane; put it in a layer instead and take over its mouse listeners.
     */
    private boolean installScaling() {
        try {
            java.lang.reflect.Field sf = applet.getClass().getDeclaredField("scroller");
            sf.setAccessible(true);
            final JScrollPane scroller = (JScrollPane) sf.get(applet);
            Object rc = applet.getClass().getField("remconsObj").get(applet);
            final Component view = (Component) rc.getClass().getField("session").get(rc);
            if (scroller == null || view == null || scroller.getViewport().getView() != view) {
                if (++waits % 10 == 1) {
                    System.err.println("[ilohost] waiting for the console picture: scroller=" + (scroller != null) + " session="
                        + (view != null ? view.getClass().getName() : null) + " viewport view="
                        + (scroller != null && scroller.getViewport().getView() != null ? scroller.getViewport().getView().getClass().getName() : null));
                }
                return false;
            }
            final ScaleUI ui = new ScaleUI(view);
            final MouseListener[] ml = view.getMouseListeners();
            final MouseMotionListener[] mm = view.getMouseMotionListeners();
            final MouseWheelListener[] mw = view.getMouseWheelListeners();
            for (MouseListener l : ml) {
                view.removeMouseListener(l);
            }
            for (MouseMotionListener l : mm) {
                view.removeMouseMotionListener(l);
            }
            for (MouseWheelListener l : mw) {
                view.removeMouseWheelListener(l);
            }
            final JLayer<Component> layer = new JLayer<Component>(view, ui);
            final Fill holder = new Fill(layer, ui);
            layer.addMouseListener(new MouseAdapter() {
                public void mousePressed(MouseEvent e) {
                    MouseEvent m = ui.map(e, layer);
                    for (MouseListener l : ml) {
                        l.mousePressed(m);
                    }
                }

                public void mouseReleased(MouseEvent e) {
                    MouseEvent m = ui.map(e, layer);
                    for (MouseListener l : ml) {
                        l.mouseReleased(m);
                    }
                }

                public void mouseClicked(MouseEvent e) {
                    MouseEvent m = ui.map(e, layer);
                    for (MouseListener l : ml) {
                        l.mouseClicked(m);
                    }
                }

                public void mouseEntered(MouseEvent e) {
                    MouseEvent m = ui.map(e, layer);
                    for (MouseListener l : ml) {
                        l.mouseEntered(m);
                    }
                }

                public void mouseExited(MouseEvent e) {
                    MouseEvent m = ui.map(e, layer);
                    for (MouseListener l : ml) {
                        l.mouseExited(m);
                    }
                }
            });
            layer.addMouseMotionListener(new MouseMotionListener() {
                public void mouseMoved(MouseEvent e) {
                    MouseEvent m = ui.map(e, layer);
                    for (MouseMotionListener l : mm) {
                        l.mouseMoved(m);
                    }
                }

                public void mouseDragged(MouseEvent e) {
                    MouseEvent m = ui.map(e, layer);
                    for (MouseMotionListener l : mm) {
                        l.mouseDragged(m);
                    }
                }
            });
            layer.addMouseWheelListener(new MouseWheelListener() {
                public void mouseWheelMoved(MouseWheelEvent e) {
                    MouseEvent m = ui.map(e, layer);
                    for (MouseWheelListener l : mw) {
                        l.mouseWheelMoved((MouseWheelEvent) m);
                    }
                }
            });
            scroller.setViewportView(holder);
            zoomChanged = new Runnable() {
                public void run() {
                    ui.factor = zoom;
                    int pol = zoom > 0 ? JScrollPane.VERTICAL_SCROLLBAR_AS_NEEDED : JScrollPane.VERTICAL_SCROLLBAR_NEVER;
                    scroller.setVerticalScrollBarPolicy(pol);
                    scroller.setHorizontalScrollBarPolicy(zoom > 0 ? JScrollPane.HORIZONTAL_SCROLLBAR_AS_NEEDED : JScrollPane.HORIZONTAL_SCROLLBAR_NEVER);
                    holder.revalidate();
                    scroller.revalidate();
                    layer.repaint();
                }
            };
            zoomChanged.run();
            // the picture's size changes with the server's resolution: lay out again when it does
            final Dimension[] last = { view.getPreferredSize() };
            new javax.swing.Timer(500, ev -> {
                Dimension d = view.getPreferredSize();
                if (d != null && !d.equals(last[0])) {
                    last[0] = d;
                    layer.revalidate();
                    layer.repaint();
                }
            }).start();
            System.err.println("[ilohost] picture scaled to the window");
            return true;
        } catch (Throwable t) {
            if (++waits % 10 == 1) {
                System.err.println("[ilohost] could not scale the picture: " + t);
            }
            return false;
        }
    }

    private int waits = 0;

    /** The applet's window is not resizable (so it cannot be docked): make it so, whenever it is not. */
    private void keepResizable() {
        for (java.awt.Frame f : java.awt.Frame.getFrames()) {
            if (f.isDisplayable() && !f.isResizable()) {
                System.err.println("[ilohost] window \"" + f.getTitle() + "\" made resizable");
                f.setResizable(true);
            }
            f.setMaximumSize(new Dimension(32767, 32767));
            f.setMinimumSize(new Dimension(200, 150));
        }
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
        // the parameters end at an empty line; stdin stays open for commands (`zoom=fit|<factor>`)
        while ((line = in.readLine()) != null && !line.isEmpty()) {
            int eq = line.indexOf('=');
            if (eq > 0) {
                host.params.put(line.substring(0, eq), line.substring(eq + 1));
            }
        }
        final BufferedReader cmds = in;
        Thread cmdThread = new Thread(() -> {
            try {
                String c;
                while ((c = cmds.readLine()) != null) {
                    if (c.startsWith("zoom=")) {
                        String v = c.substring(5).trim();
                        try {
                            zoom = v.equals("fit") ? 0 : v.equals("stretch") ? -1 : Math.max(0.25, Math.min(4, Double.parseDouble(v)));
                        } catch (NumberFormatException e) {
                            continue;
                        }
                        final Runnable r = zoomChanged;
                        if (r != null) {
                            SwingUtilities.invokeLater(r);
                        }
                    }
                }
            } catch (java.io.IOException e) {
                // bobcat went away: the shutdown hook closes the session
            }
        }, "bobcat-commands");
        cmdThread.setDaemon(true);
        cmdThread.start();
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
        if (!"0".equals(System.getenv("BOBCAT_ILO_SCALE"))) {
            // the applet builds its window on its own thread: keep trying until it is there
            final boolean[] done = { false };
            javax.swing.Timer t = new javax.swing.Timer(500, null);
            t.addActionListener(e -> {
                host.keepResizable();
                if (!done[0]) {
                    done[0] = host.installScaling();
                }
            });
            t.start();
        }
        System.err.println("[ilohost] starting the applet");
        host.applet.start();
        System.err.println("[ilohost] applet started");
        Thread.sleep(Long.MAX_VALUE);
    }
}
