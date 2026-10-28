package openhc.mc;

import net.minecraft.launchwrapper.IClassTransformer;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.tree.AbstractInsnNode;
import org.objectweb.asm.tree.ClassNode;
import org.objectweb.asm.tree.FieldInsnNode;
import org.objectweb.asm.tree.FieldNode;
import org.objectweb.asm.tree.InsnList;
import org.objectweb.asm.tree.InsnNode;
import org.objectweb.asm.tree.IntInsnNode;
import org.objectweb.asm.tree.JumpInsnNode;
import org.objectweb.asm.tree.MethodInsnNode;
import org.objectweb.asm.tree.MethodNode;
import org.objectweb.asm.tree.VarInsnNode;

/*
 * Every tick, Minecraft 1.7.10 calls Chunk.func_150809_p (obfuscated apx.p) on each
 * loaded chunk whose light isn't populated yet, from Chunk.func_150804_b (apx.b(Z)).
 * It relights the chunk column by column and gives up, marking the chunk unlit again,
 * at the first column whose 17-block surroundings aren't loaded. Chunks at the edge of
 * the loaded area keep failing, so the integrated server redoes and throws away that
 * lighting every tick: in a jungle, on a slow CPU, that is a whole core, and the server
 * falls behind ("Can't keep up!").
 *
 * This keeps the logic and only spaces the retries out: a chunk still tries on its
 * first tick, then on every RETRY-th tick until it succeeds. The server only sends a
 * chunk to the client once its light is populated, so the interval is kept short.
 */
public class LightTransformer implements IClassTransformer {
	static final int RETRY = 10;	// ticks (0.5 s)
	static final String FIELD = "openhcLightRetry";

	public byte[] transform(String name, String transformedName, byte[] bytes) {
		if (bytes == null || !"apx".equals(name))
			return bytes;
		ClassNode cn = new ClassNode();
		new ClassReader(bytes).accept(cn, 0);
		for (Object o : cn.methods) {
			MethodNode m = (MethodNode) o;
			if (!"b".equals(m.name) || !"(Z)V".equals(m.desc))
				continue;
			// ... if (!this.l && this.k) this.p(); with the k test's "ifeq" right before "aload_0; invokevirtual p"
			for (AbstractInsnNode i = m.instructions.getFirst(); i != null; i = i.getNext()) {
				if (i.getOpcode() != Opcodes.INVOKEVIRTUAL)
					continue;
				MethodInsnNode c = (MethodInsnNode) i;
				if (!"apx".equals(c.owner) || !"p".equals(c.name) || !"()V".equals(c.desc))
					continue;
				AbstractInsnNode self = real(c.getPrevious()), test = self == null ? null : real(self.getPrevious());
				if (self == null || self.getOpcode() != Opcodes.ALOAD || ((VarInsnNode) self).var != 0
						|| test == null || test.getOpcode() != Opcodes.IFEQ)
					break;
				// if (this.openhcLightRetry++ % RETRY != 0) skip the call
				InsnList gate = new InsnList();
				gate.add(new VarInsnNode(Opcodes.ALOAD, 0));
				gate.add(new InsnNode(Opcodes.DUP));
				gate.add(new FieldInsnNode(Opcodes.GETFIELD, "apx", FIELD, "I"));
				gate.add(new InsnNode(Opcodes.DUP_X1));
				gate.add(new InsnNode(Opcodes.ICONST_1));
				gate.add(new InsnNode(Opcodes.IADD));
				gate.add(new FieldInsnNode(Opcodes.PUTFIELD, "apx", FIELD, "I"));
				gate.add(new IntInsnNode(Opcodes.BIPUSH, RETRY));
				gate.add(new InsnNode(Opcodes.IREM));
				gate.add(new JumpInsnNode(Opcodes.IFNE, ((JumpInsnNode) test).label));
				m.instructions.insertBefore(self, gate);
				cn.fields.add(new FieldNode(Opcodes.ACC_PRIVATE | Opcodes.ACC_TRANSIENT, FIELD, "I", null, null));
				ClassWriter cw = new ClassWriter(ClassWriter.COMPUTE_MAXS);
				cn.accept(cw);
				System.out.println("[openhc] light fix: Chunk light retries every " + RETRY + " ticks");
				return cw.toByteArray();
			}
			break;
		}
		System.out.println("[openhc] light fix: Chunk.b(Z) is not the expected 1.7.10 code, left alone");
		return bytes;
	}

	// the nearest real instruction at or before i, skipping labels, line numbers and frames
	static AbstractInsnNode real(AbstractInsnNode i) {
		while (i != null && i.getOpcode() < 0)
			i = i.getPrevious();
		return i;
	}
}
