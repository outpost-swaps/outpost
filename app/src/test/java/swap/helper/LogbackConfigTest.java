package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;

import java.io.InputStream;
import javax.xml.parsers.DocumentBuilderFactory;
import org.junit.Test;
import org.w3c.dom.Document;
import org.w3c.dom.Element;
import org.w3c.dom.NodeList;

/**
 * Without a logback.xml the default is DEBUG for everything, which printed whole Jetty HTTP
 * requests, Dojo API key included, to the console and to saved logs.
 */
public class LogbackConfigTest {

    private static Document load() throws Exception {
        try (InputStream in = LogbackConfigTest.class.getResourceAsStream("/logback.xml")) {
            assertNotNull("logback.xml must be on the classpath", in);
            return DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(in);
        }
    }

    private static String levelOf(Document doc, String logger) {
        NodeList loggers = doc.getElementsByTagName("logger");
        for (int i = 0; i < loggers.getLength(); i++) {
            Element element = (Element) loggers.item(i);
            if (logger.equals(element.getAttribute("name"))) {
                return element.getAttribute("level");
            }
        }
        return null;
    }

    @Test
    public void theRootLevelIsNotDebug() throws Exception {
        Element root = (Element) load().getElementsByTagName("root").item(0);
        assertEquals("INFO", root.getAttribute("level"));
    }

    @Test
    public void httpTransportsThatDumpRequestsAreQuiet() throws Exception {
        Document doc = load();
        for (String logger : new String[] {"org.eclipse.jetty", "io.netty"}) {
            assertEquals(logger, "WARN", levelOf(doc, logger));
        }
    }
}
