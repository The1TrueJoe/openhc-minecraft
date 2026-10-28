package openhc.mc;

import java.io.File;
import java.util.ArrayList;
import java.util.List;
import net.minecraft.launchwrapper.ITweaker;
import net.minecraft.launchwrapper.Launch;
import net.minecraft.launchwrapper.LaunchClassLoader;

/*
 * openHC's launchwrapper tweaker: registers the class transformers in this package.
 * The launcher passes it after OptiFine's tweaker when OptiFine is installed, and
 * alone otherwise. Launchwrapper hands the game whatever the tweakers' getLaunchArguments
 * return, in order, and keeps --version/--gameDir/--assetsDir for itself, so when this is
 * the only tweaker it gives those back along with the rest.
 */
public class Tweaker implements ITweaker {
	private List<String> args;
	private File gameDir, assetsDir;
	private String version;

	public void acceptOptions(List<String> args, File gameDir, File assetsDir, String version) {
		this.args = new ArrayList<String>(args);
		this.gameDir = gameDir;
		this.assetsDir = assetsDir;
		this.version = version;
	}

	public void injectIntoClassLoader(LaunchClassLoader classLoader) {
		classLoader.registerTransformer("openhc.mc.LightTransformer");
	}

	public String getLaunchTarget() {
		return "net.minecraft.client.main.Main";
	}

	public String[] getLaunchArguments() {
		List<?> given = (List<?>) Launch.blackboard.get("ArgumentList");
		if (given != null && !given.isEmpty())
			return new String[0];	// an earlier tweaker (OptiFine's) passed the game's arguments
		List<String> a = new ArrayList<String>(args);
		if (version != null && !a.contains("--version")) {
			a.add("--version");
			a.add(version);
		}
		if (gameDir != null && !a.contains("--gameDir")) {
			a.add("--gameDir");
			a.add(gameDir.getAbsolutePath());
		}
		if (assetsDir != null && !a.contains("--assetsDir")) {
			a.add("--assetsDir");
			a.add(assetsDir.getAbsolutePath());
		}
		return a.toArray(new String[a.size()]);
	}
}
