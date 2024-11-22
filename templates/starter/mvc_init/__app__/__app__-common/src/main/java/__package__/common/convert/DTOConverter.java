package __package__.common.convert;

import java.util.List;

/**
 * DTO 对象互转模板类
 *
 * @author  weirenyan
 */
public interface DTOConverter<DTO, DO> {

    /**
     * 类型转换
     *
     * @param DTO 输入
     * @return 输出
     */
    DO convert(DTO DTO);

    /**
     * 类型转换
     *
     * @param entities 输入
     * @return 输出
     */
    List<DO> batchConvert(List<DTO> entities);

    /**
     * 类型转换
     *
     * @param entity 输入
     * @return 输出
     */
    DTO reverseConvert(DO entity);

    /**
     * 类型转换
     *
     * @param entities 输入
     * @return 输出
     */
    List<DTO> batchReverseConvert(List<DO> entities);


}
